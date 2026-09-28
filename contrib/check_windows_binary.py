#!/usr/bin/env python3
"""Check the built GUI's stack reserve and embedded icon resource types.

PE layout: https://learn.microsoft.com/en-us/windows/win32/debug/pe-format
This reads the executable as data; it never launches the wallet.
"""

from pathlib import Path
import struct
import sys


def check_binary(path):
    data = Path(path).read_bytes()

    def read(fmt, offset):
        return struct.unpack_from('<' + fmt, data, offset)[0]

    if data[:2] != b'MZ':
        raise ValueError('not a DOS/PE executable')
    pe = read('I', 0x3C)
    if data[pe:pe + 4] != b'PE\0\0':
        raise ValueError('missing PE signature')
    optional = pe + 24
    magic = read('H', optional)
    if magic not in (0x10B, 0x20B):
        raise ValueError('unsupported PE optional header')
    stack = read('Q' if magic == 0x20B else 'I', optional + 72)
    if stack != 8_000_000:
        raise ValueError(f'expected 8,000,000-byte stack reserve, found {stack:,}')
    directories = optional + (112 if magic == 0x20B else 96)
    if read('I', directories - 4) < 3:
        raise ValueError('missing resource data directory')
    resource_rva = read('I', directories + 16)
    resource_size = read('I', directories + 20)
    if not resource_rva or resource_size < 16:
        raise ValueError('missing resource table')
    sections = optional + read('H', pe + 20)
    for index in range(read('H', pe + 6)):
        section = sections + index * 40
        address = read('I', section + 12)
        raw_size = read('I', section + 16)
        if address <= resource_rva < address + raw_size:
            root = read('I', section + 20) + resource_rva - address
            names = read('H', root + 12)
            ids = read('H', root + 14)
            if 16 + 8 * (names + ids) > resource_size:
                raise ValueError('truncated resource directory')
            types = {read('I', root + 16 + 8 * i) for i in range(names, names + ids)}
            if not {3, 14}.issubset(types):  # RT_ICON, RT_GROUP_ICON
                raise ValueError('missing application icon resources')
            print(f'{path}: stack reserve={stack:,}; icon and icon-group resources present')
            return
    raise ValueError('resource directory is not backed by a file section')


if __name__ == '__main__':
    try:
        check_binary(sys.argv[1])
    except (IndexError, OSError, ValueError, struct.error) as error:
        sys.exit(f'Windows binary check failed: {error}')
