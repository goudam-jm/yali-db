//! HTTP/1 framing belongs to Hyper. Limits are enforced before spawning work.
use crate::{MAX_BODY, constant_eq, dispatch, failure};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
    header,
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    env, io,
    net::SocketAddr,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpListener,
    sync::{Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
    time::{Instant, Sleep, timeout},
};
use yalidb_core::Db;

type Reply = Response<Full<Bytes>>;
type AnyError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Clone)]
struct Config {
    connections: usize,
    queue: usize,
    header: Duration,
    body: Duration,
    write: Duration,
    request: Duration,
    lifetime: Duration,
    shutdown: Duration,
}
impl Config {
    fn load() -> Result<Self, AnyError> {
        fn number(name: &str, default: u64, min: u64, max: u64) -> Result<u64, AnyError> {
            let value = match env::var(name) {
                Ok(v) => v.parse::<u64>().map_err(|_| format!("invalid {name}"))?,
                Err(env::VarError::NotPresent) => default,
                Err(_) => return Err(format!("invalid {name}").into()),
            };
            if !(min..=max).contains(&value) {
                return Err(format!("{name} must be {min}..={max}").into());
            }
            Ok(value)
        }
        fn ms(name: &str, default: u64) -> Result<Duration, AnyError> {
            Ok(Duration::from_millis(number(name, default, 50, 60_000)?))
        }
        Ok(Self {
            connections: number("YALIDB_MAX_CONNECTIONS", 64, 1, 4096)? as usize,
            queue: number("YALIDB_QUEUE_CAPACITY", 16, 1, 1024)? as usize,
            header: ms("YALIDB_HEADER_TIMEOUT_MS", 5000)?,
            body: ms("YALIDB_BODY_TIMEOUT_MS", 5000)?,
            write: ms("YALIDB_WRITE_TIMEOUT_MS", 5000)?,
            request: ms("YALIDB_REQUEST_TIMEOUT_MS", 10_000)?,
            lifetime: Duration::from_millis(number(
                "YALIDB_CONNECTION_LIFETIME_MS",
                60_000,
                100,
                3_600_000,
            )?),
            shutdown: ms("YALIDB_SHUTDOWN_TIMEOUT_MS", 30_000)?,
        })
    }
}

struct Job {
    path: String,
    body: Vec<u8>,
    expires: Instant,
    reply: oneshot::Sender<(u16, Value)>,
}
fn process_job(db: &Db, ready: &AtomicBool, job: Job) {
    if job.reply.is_closed() || Instant::now() >= job.expires {
        return;
    }
    let result = match dispatch(db, &job.path, &job.body) {
        Ok(value) => (200, value),
        Err(e) => failure(e),
    };
    ready.store(db.stats().is_ok_and(|s| s.healthy), Ordering::Release);
    let _ = job.reply.send(result);
}
#[derive(Clone)]
struct App {
    jobs: mpsc::Sender<Job>,
    auth: Arc<str>,
    ready: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    config: Config,
}

fn response(status: u16, value: Value) -> Reply {
    let mut response = Response::new(Full::new(Bytes::from(value.to_string())));
    *response.status_mut() =
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    if status >= 400 {
        headers.insert(
            header::CONNECTION,
            header::HeaderValue::from_static("close"),
        );
    }
    response
}
fn error(status: u16, code: &str) -> Reply {
    response(status, json!({"error":code}))
}

async fn handle(request: Request<Incoming>, app: App) -> Reply {
    let headers = request.headers();
    let mut auth = headers.get_all(header::AUTHORIZATION).iter();
    if !auth
        .next()
        .is_some_and(|v| constant_eq(v.as_bytes(), app.auth.as_bytes()))
        || auth.next().is_some()
    {
        return error(401, "unauthorized");
    }
    if app.stopping.load(Ordering::Acquire) {
        return error(503, "shutting_down");
    }
    // Reject framing ambiguities and unsupported request forms before consuming a body.
    if headers.get_all(header::HOST).iter().count() != 1
        || request.uri().scheme().is_some()
        || request.uri().authority().is_some()
    {
        return error(400, "invalid_request_target_or_host");
    }
    if headers.contains_key(header::TRANSFER_ENCODING) {
        return error(400, "transfer_encoding_unsupported");
    }
    if headers.get_all(header::CONTENT_LENGTH).iter().count() > 1 {
        return error(400, "duplicate_content_length");
    }
    if headers.contains_key(header::EXPECT) {
        return error(417, "expect_unsupported");
    }
    let length = match headers.get(header::CONTENT_LENGTH) {
        Some(v) => match v
            .to_str()
            .ok()
            .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|v| v.parse::<usize>().ok())
        {
            Some(n) => Some(n),
            None => return error(400, "invalid_content_length"),
        },
        None => None,
    };
    let path = request
        .uri()
        .path_and_query()
        .map_or("", |p| p.as_str())
        .to_owned();
    if request.method() == hyper::Method::GET
        && ["/health/live", "/health/ready"].contains(&path.as_str())
    {
        if length.is_some_and(|n| n != 0) {
            return error(400, "health_body_unsupported");
        }
        let ready = app.ready.load(Ordering::Acquire) && !app.jobs.is_closed();
        return if path == "/health/live" {
            response(200, json!({"live":true}))
        } else {
            response(if ready { 200 } else { 503 }, json!({"ready":ready}))
        };
    }
    if !["/v1/get", "/v1/put", "/v1/delete", "/v1/scan"].contains(&path.as_str()) {
        return error(404, "not_found");
    }
    if request.method() != hyper::Method::POST {
        return error(405, "method_not_allowed");
    }
    if headers.get_all(header::CONTENT_TYPE).iter().count() != 1
        || !headers
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|v| {
                v.split(';')
                    .next()
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
            })
    {
        return error(415, "expected_application_json");
    }
    let Some(length) = length else {
        return error(411, "content_length_required");
    };
    if length > MAX_BODY {
        return error(413, "body_too_large");
    }
    let body = match timeout(
        app.config.body,
        Limited::new(request.into_body(), MAX_BODY).collect(),
    )
    .await
    {
        Ok(Ok(body)) => body.to_bytes(),
        Ok(Err(_)) => return error(400, "invalid_body"),
        Err(_) => return error(408, "body_timeout"),
    };
    if body.len() != length {
        return error(400, "body_length_mismatch");
    }
    execute(app, path, body.to_vec()).await
}

async fn execute(app: App, path: String, body: Vec<u8>) -> Reply {
    let (send, receive) = oneshot::channel();
    let expires = Instant::now() + app.config.request;
    let job = Job {
        path,
        body,
        expires,
        reply: send,
    };
    match app.jobs.try_send(job) {
        Ok(()) => (),
        Err(mpsc::error::TrySendError::Full(_)) => return error(429, "storage_queue_full"),
        Err(_) => return error(503, "storage_unavailable"),
    }
    match tokio::time::timeout_at(expires, receive).await {
        Ok(Ok((status, payload))) => response(status, payload),
        _ => response(
            503,
            json!({"error":"storage_timeout_or_unavailable","mutation_outcome_may_be_unknown":true}),
        ),
    }
}

pub async fn serve() -> Result<(), AnyError> {
    let mut args = env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: yalidb-server DIRECTORY [127.0.0.1:7419]; set YALIDB_TOKEN (>=32 bytes)")?;
    let addr: SocketAddr = args
        .next()
        .unwrap_or_else(|| "127.0.0.1:7419".into())
        .parse()?;
    if args.next().is_some() || !addr.ip().is_loopback() {
        return Err("only an explicit loopback address is supported".into());
    }
    let token = env::var("YALIDB_TOKEN").map_err(|_| "YALIDB_TOKEN is required")?;
    if token.len() < 32 || token.bytes().any(|b| b.is_ascii_control()) {
        return Err("YALIDB_TOKEN must be at least 32 bytes without control characters".into());
    }
    let config = Config::load()?;
    let db = Db::open(path)?;
    let listener = TcpListener::bind(addr).await?;
    // Install both signal listeners before startup is advertised to supervisors/tests.
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let (jobs, mut receiver) = mpsc::channel::<Job>(config.queue);
    let ready = Arc::new(AtomicBool::new(true));
    let worker_ready = ready.clone();
    let (worker_done, done) = oneshot::channel();
    let worker = std::thread::Builder::new()
        .name("yalidb-storage".into())
        .spawn(move || {
            while let Some(job) = receiver.blocking_recv() {
                // Skipped requests have not started. Once dispatch begins it is never canceled mid-write.
                process_job(&db, &worker_ready, job);
            }
            drop(db);
            let _ = worker_done.send(());
        })?;
    let app = App {
        jobs,
        auth: format!("Bearer {token}").into(),
        ready,
        stopping: Arc::new(AtomicBool::new(false)),
        config: config.clone(),
    };
    let slots = Arc::new(Semaphore::new(config.connections));
    let mut tasks = JoinSet::new();
    let (stop, _) = watch::channel(false);
    eprintln!(
        "YaliDB experimental loopback service at {}; bounded HTTP enabled",
        listener.local_addr()?
    );
    loop {
        tokio::select! {
            biased;
            _=terminate.recv()=>break,
            _=interrupt.recv()=>break,
            Some(result)=tasks.join_next(), if !tasks.is_empty()=>{if result.is_err() {eprintln!("connection task failed");}},
            accepted=listener.accept()=>{
                let (stream,_)=accepted?;
                // No waiting connection queue and no task/thread per refused socket.
                let Ok(permit)=slots.clone().try_acquire_owned() else {drop(stream);continue;};
                stream.set_nodelay(true)?;
                let app=app.clone();let mut stopping=stop.subscribe();
                tasks.spawn(async move {
                    let _permit=permit;
                    let count=Arc::new(AtomicUsize::new(0));
                    let io=TokioIo::new(WriteDeadline::new(stream,app.config.write));
                    let mut builder=http1::Builder::new();
                    builder.timer(TokioTimer::new()).header_read_timeout(app.config.header)
                        .max_headers(32).max_buf_size(16*1024).keep_alive(true);
                    let lifetime=app.config.lifetime;
                    let service=service_fn(move |request| {
                        let app=app.clone();let ordinal=count.fetch_add(1,Ordering::Relaxed);
                        async move {
                            let mut response=handle(request,app).await;
                            if ordinal>=99 {response.headers_mut().insert(header::CONNECTION,header::HeaderValue::from_static("close"));}
                            Ok::<_,Infallible>(response)
                        }
                    });
                    let connection=builder.serve_connection(io,service);
                    tokio::pin!(connection);
                    tokio::select! {
                        _=&mut connection=>(),
                        _=tokio::time::sleep(lifetime)=>(),
                        _=stopping.changed()=>{connection.as_mut().graceful_shutdown();let _=connection.await;},
                    }
                });
            }
        }
    }
    drop(listener);
    app.stopping.store(true, Ordering::Release);
    app.ready.store(false, Ordering::Release);
    let _ = stop.send(true);
    let deadline = Instant::now() + config.shutdown;
    if tokio::time::timeout_at(deadline, async {
        while tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
    drop(app); // Close the storage queue after every request task releases its sender.
    if tokio::time::timeout_at(deadline, done).await.is_err() {
        // An OS write/fsync cannot safely be canceled. Exit with failure; outcomes may be unknown.
        return Err(
            "shutdown deadline exceeded while draining storage; reopen to resolve outcomes".into(),
        );
    }
    worker.join().map_err(|_| "storage worker panicked")?;
    Ok(())
}

/// One absolute timer across all writes until a flush completes, not a timer reset per byte.
struct WriteDeadline<T> {
    inner: T,
    deadline: Option<Pin<Box<Sleep>>>,
    duration: Duration,
}
impl<T> WriteDeadline<T> {
    fn new(inner: T, duration: Duration) -> Self {
        Self {
            inner,
            deadline: None,
            duration,
        }
    }
    fn check(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        use std::future::Future;
        let timer = self
            .deadline
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(self.duration)));
        if timer.as_mut().poll(cx).is_ready() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "response write deadline",
            ))
        } else {
            Ok(())
        }
    }
}
impl<T: AsyncRead + Unpin> AsyncRead for WriteDeadline<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buffer)
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for WriteDeadline<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.check(cx)?;
        Pin::new(&mut this.inner).poll_write(cx, bytes)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.deadline.is_some() {
            this.check(cx)?;
        }
        let result = Pin::new(&mut this.inner).poll_flush(cx);
        if result.is_ready() {
            this.deadline = None;
        }
        result
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn app(capacity: usize) -> (App, mpsc::Receiver<Job>) {
        let (jobs, receiver) = mpsc::channel(capacity);
        let config = Config {
            connections: 2,
            queue: capacity,
            header: Duration::from_millis(50),
            body: Duration::from_millis(50),
            write: Duration::from_millis(50),
            request: Duration::from_millis(50),
            lifetime: Duration::from_secs(1),
            shutdown: Duration::from_secs(1),
        };
        (
            App {
                jobs,
                auth: Arc::from("test"),
                ready: Arc::new(AtomicBool::new(true)),
                stopping: Arc::new(AtomicBool::new(false)),
                config,
            },
            receiver,
        )
    }
    fn mutation(expires: Instant) -> (Job, oneshot::Receiver<(u16, Value)>) {
        let (reply, receive) = oneshot::channel();
        (
            Job {
                path: "/v1/put".into(),
                body: br#"{"key":"aw","value":"dg"}"#.to_vec(),
                expires,
                reply,
            },
            receive,
        )
    }
    #[tokio::test]
    async fn bounded_queue_rejects_overload_without_enqueuing() {
        let (app, mut receiver) = app(1);
        let (job, _reply) = mutation(Instant::now() + Duration::from_secs(1));
        app.jobs.try_send(job).unwrap();
        let response = execute(app, "/v1/put".into(), vec![]).await;
        assert_eq!(response.status(), 429);
        receiver.try_recv().unwrap();
        assert!(receiver.try_recv().is_err());
    }
    #[tokio::test]
    async fn response_deadline_is_ambiguous_and_expired_jobs_do_not_mutate() {
        let (app, mut receiver) = app(1);
        let response = execute(
            app,
            "/v1/put".into(),
            br#"{"key":"aw","value":"dg"}"#.to_vec(),
        )
        .await;
        assert_eq!(response.status(), 503);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(
            serde_json::from_slice::<Value>(&body).unwrap()["mutation_outcome_may_be_unknown"]
                .as_bool()
                .unwrap()
        );
        let db = Db::memory();
        process_job(&db, &AtomicBool::new(true), receiver.try_recv().unwrap());
        assert!(db.get(b"k").unwrap().is_none());
        let (job, _receive) = mutation(Instant::now() - Duration::from_millis(1));
        process_job(&db, &AtomicBool::new(true), job);
        assert_eq!(db.stats().unwrap().sequence, 0);
    }
    // Always making a little progress must not reset an absolute write deadline.
    struct SlowWriter {
        tick: Pin<Box<Sleep>>,
    }
    impl AsyncWrite for SlowWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            use std::future::Future;
            let this = self.get_mut();
            if this.tick.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
            this.tick = Box::pin(tokio::time::sleep(Duration::from_millis(10)));
            Poll::Ready(Ok(1))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    #[tokio::test]
    async fn write_deadline_covers_dribbling_writes_and_flush() {
        let slow = SlowWriter {
            tick: Box::pin(tokio::time::sleep(Duration::from_millis(1))),
        };
        let mut writer = WriteDeadline::new(slow, Duration::from_millis(50));
        let error = timeout(Duration::from_secs(1), writer.write_all(&[1; 100]))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        let slow = SlowWriter {
            tick: Box::pin(tokio::time::sleep(Duration::from_millis(1))),
        };
        let mut writer = WriteDeadline::new(slow, Duration::from_millis(50));
        writer.write_all(&[1]).await.unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), writer.flush())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }
}
