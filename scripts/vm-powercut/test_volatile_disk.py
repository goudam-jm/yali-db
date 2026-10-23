"""Validate the persistence oracle independently of the database and guest."""
import io
import os
import socket
import struct
from pathlib import Path
import tempfile
import unittest
from volatile_disk import Disk, receive


class CacheTests(unittest.TestCase):
    def test_only_flush_persists_and_overlapping_writes_are_visible(self):
        with tempfile.TemporaryDirectory() as folder:
            image = Path(folder) / 'fresh.raw'
            disk = Disk(image, Path(folder) / 'socket', io.StringIO())
            disk.write(0, b'a' * 1024)
            disk.write(512, b'b' * 512)
            self.assertEqual(disk.read(0, 1024), b'a' * 512 + b'b' * 512)
            self.assertEqual(os.pread(disk.fd, 1024, 0), bytes(1024))
            disk.flush()
            disk.write(0, b'c' * 512)
            evidence = disk.power_cut()
            self.assertEqual(evidence['discarded_bytes'], 512)
            with image.open('rb') as stream: self.assertEqual(stream.read(1024), b'a' * 512 + b'b' * 512)

    def test_lying_flush_negative_control_drops_acknowledged_data(self):
        with tempfile.TemporaryDirectory() as folder:
            image = Path(folder) / 'fresh.raw'
            disk = Disk(image, Path(folder) / 'socket', io.StringIO())
            disk.write(0, b'a' * 512); disk.flush()
            disk.ignore_flush = True
            disk.write(0, b'b' * 512); disk.flush()
            self.assertEqual(disk.read(0, 512), b'b' * 512)
            evidence = disk.power_cut()
            self.assertEqual(evidence['counts']['ignored_flush'], 1)
            with image.open('rb') as stream: self.assertEqual(stream.read(512), b'a' * 512)

    def test_wire_fua_persists_and_unsupported_command_is_rejected(self):
        with tempfile.TemporaryDirectory() as folder:
            image = Path(folder) / 'fresh.raw'
            disk = Disk(image, Path(folder) / 'socket', io.StringIO()); disk.serve()
            with socket.socket(socket.AF_UNIX) as client:
                client.connect(str(Path(folder) / 'socket'))
                handshake = receive(client, 152)
                self.assertEqual(handshake[:8], b'NBDMAGIC')
                client.sendall(struct.pack('>IHHQQI', 0x25609513, 1, 1, 42, 0, 512) + b'a' * 512)
                self.assertEqual(struct.unpack('>IIQ', receive(client, 16)), (0x67446698, 0, 42))
                self.assertEqual(os.pread(disk.fd, 512, 0), b'a' * 512)
                client.sendall(struct.pack('>IHHQQI', 0x25609513, 0, 99, 43, 0, 0))
                self.assertNotEqual(struct.unpack('>IIQ', receive(client, 16))[1], 0)
                client.sendall(struct.pack('>IHHQQI', 0x25609513, 0, 0, 44, 0, 512))
                self.assertEqual(struct.unpack('>IIQ', receive(client, 16)), (0x67446698, 0, 44))
                self.assertEqual(receive(client, 512), b'a' * 512)
            disk.power_cut()

    def test_cut_at_wire_write_discards_it_before_reply(self):
        with tempfile.TemporaryDirectory() as folder:
            image = Path(folder) / 'fresh.raw'
            disk = Disk(image, Path(folder) / 'socket', io.StringIO()); disk.serve()
            disk.arm('write')
            with socket.socket(socket.AF_UNIX) as client:
                client.connect(str(Path(folder) / 'socket')); receive(client, 152)
                client.sendall(struct.pack('>IHHQQI', 0x25609513, 0, 1, 45, 0, 512) + b'z' * 512)
                self.assertTrue(disk.hit.wait(2))
                self.assertEqual(disk.power_cut()['discarded_bytes'], 512)
                self.assertEqual(client.recv(16), b'')
            with image.open('rb') as stream: self.assertEqual(stream.read(512), bytes(512))

    def test_refuses_existing_image(self):
        with tempfile.TemporaryDirectory() as folder:
            image = Path(folder) / 'fresh.raw'; image.write_bytes(b'preserve')
            with self.assertRaises(FileExistsError): Disk(image, Path(folder) / 'socket', io.StringIO())
            self.assertEqual(image.read_bytes(), b'preserve')


if __name__ == '__main__': unittest.main()
