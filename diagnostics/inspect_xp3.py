#!/usr/bin/env python3
"""Read-only bounded XP3 script inspection; never executes game scripts."""
import argparse
import fnmatch
import struct
import zlib
from pathlib import Path, PurePosixPath


def chunks(data):
    offset = 0
    while offset < len(data):
        assert offset + 12 <= len(data), "truncated chunk header"
        tag, length = struct.unpack_from('<4sQ', data, offset)
        offset += 12
        assert offset + length <= len(data), "truncated chunk"
        yield tag, data[offset:offset + length]
        offset += length


class Archive:
    def __init__(self, path):
        self.path = path
        self.entries = {}
        with path.open('rb') as stream:
            assert stream.read(11) == b'XP3\r\n \n\x1a\x8bg\x01'
            pointer, = struct.unpack('<Q', stream.read(8))
            seen = set()
            while pointer:
                assert pointer not in seen and len(seen) < 64
                seen.add(pointer)
                stream.seek(pointer)
                flags = stream.read(1)[0]
                assert flags & ~0x81 == 0
                stored, = struct.unpack('<Q', stream.read(8))
                original = struct.unpack('<Q', stream.read(8))[0] if flags & 1 else stored
                assert original < 64 * 1024 * 1024 and stored < 64 * 1024 * 1024
                data = stream.read(stored)
                pointer = struct.unpack('<Q', stream.read(8))[0] if flags & 0x80 else 0
                if flags & 1:
                    data = zlib.decompress(data)
                assert len(data) == original
                for tag, body in chunks(data):
                    assert tag == b'File'
                    fields = dict(chunks(body))
                    info = fields[b'info']
                    flags, size, stored, units = struct.unpack_from('<IQQH', info)
                    assert flags == 0
                    name = info[22:22 + units * 2].decode('utf-16le').replace('\\', '/')
                    segments = list(struct.iter_unpack('<IQQQ', fields[b'segm']))
                    checksum, = struct.unpack('<I', fields[b'adlr'])
                    self.entries[name.lower()] = (name, size, checksum, segments)

    def read(self, key):
        name, size, checksum, segments = self.entries[key]
        assert size <= 16 * 1024 * 1024, "selected script too large"
        result = bytearray()
        with self.path.open('rb') as stream:
            for flags, offset, original, stored in segments:
                assert flags in (0, 1) and original <= size and stored <= 16 * 1024 * 1024
                stream.seek(offset)
                data = stream.read(stored)
                if flags == 1:
                    data = zlib.decompress(data)
                assert len(data) == original
                result.extend(data)
        assert len(result) == size and zlib.adler32(result) == checksum, name
        return bytes(result)


def decode(data):
    if data.startswith((b'\xff\xfe', b'\xfe\xff')):
        return data.decode('utf-16')
    try:
        return data.decode('utf-8-sig')
    except UnicodeDecodeError:
        return data.decode('cp932')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--pattern', default='scenario/*.ks')
    parser.add_argument('--out', type=Path)
    parser.add_argument('--base-only', action='store_true', help='use only data.xp3 and patch.xp3')
    args = parser.parse_args()
    overlay = {}
    for name in ('data.xp3', 'adult.xp3', 'patch.xp3'):
        if name == 'adult.xp3' and args.base_only:
            continue
        path = args.root / name
        if path.is_file():
            archive = Archive(path)
            overlay.update({key: archive for key in archive.entries})
    for key in sorted(overlay):
        if not fnmatch.fnmatchcase(key, args.pattern.lower()):
            continue
        archive = overlay[key]
        name, size, _, _ = archive.entries[key]
        print(f'{name}\t{size}')
        if args.out is not None:
            parts = PurePosixPath(name).parts
            assert not PurePosixPath(name).is_absolute() and '..' not in parts
            target = args.out.joinpath(*parts)
            assert not target.exists(), f'refuse existing output {target}'
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(decode(archive.read(key)), encoding='utf-8')


if __name__ == '__main__':
    main()
