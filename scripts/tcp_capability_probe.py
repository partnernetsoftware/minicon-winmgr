"""Observe TCP EStats on one owned loopback connection; never initiates UAC.

This is a capability observation, not a process bandwidth or load certification.
Only this newly-created socket's collection flag may be changed; it is restored.
"""
import ctypes as c
import json
import socket
import time


class Row(c.Structure):
    _fields_ = [(name, c.c_uint32) for name in
                ('state', 'local_addr', 'local_port', 'remote_addr', 'remote_port')]


class Data(c.Structure):
    _fields_ = [(name, c.c_uint64) for name in
                ('bytes_out', 'segments_out', 'bytes_in', 'segments_in', 'all_out', 'all_in')]
    _fields_ += [(name, c.c_uint32) for name in
                 ('soft_errors', 'soft_reason', 'snd_una', 'snd_nxt', 'snd_max')]
    _fields_ += [('acked', c.c_uint64), ('rcv_nxt', c.c_uint32), ('received', c.c_uint64)]


def main():
    assert c.sizeof(Data) == 96
    api = c.WinDLL('iphlpapi')
    table = api.GetTcpTable
    table.argtypes = [c.c_void_p, c.POINTER(c.c_uint32), c.c_bool]
    table.restype = c.c_uint32
    get = api.GetPerTcpConnectionEStats
    get.argtypes = [c.POINTER(Row), c.c_int, c.c_void_p, c.c_uint32, c.c_uint32,
                   c.c_void_p, c.c_uint32, c.c_uint32, c.c_void_p, c.c_uint32, c.c_uint32]
    get.restype = c.c_uint32
    set_stats = api.SetPerTcpConnectionEStats
    set_stats.argtypes = [c.POINTER(Row), c.c_int, c.c_void_p,
                         c.c_uint32, c.c_uint32, c.c_uint32]
    set_stats.restype = c.c_uint32
    result = dict(owned_loopback=True, ipv4_tcp_only=True, counter_verified=False,
                  udp_covered=False, short_connection_coverage=False, sustained_cost_verified=False)
    listener = socket.socket()
    client = socket.socket()
    server = None
    row = None
    changed = False
    try:
        listener.bind(('127.0.0.1', 0))
        listener.listen(1)
        client.settimeout(3)
        client.connect(listener.getsockname())
        server, _ = listener.accept()
        server.settimeout(3)
        for _ in range(4):
            size = c.c_uint32()
            assert table(None, c.byref(size), False) == 122
            assert size.value <= 16 * 1024 * 1024
            buffer = c.create_string_buffer(size.value)
            status = table(buffer, c.byref(size), False)
            if status != 122:
                assert status == 0
                break
        else:
            result['table_churn_prevented_observation'] = True
            return result
        count = c.cast(buffer, c.POINTER(c.c_uint32))[0]
        assert 4 + count * c.sizeof(Row) <= size.value
        rows = (Row * count).from_buffer(buffer, 4)
        row = next(r for r in rows
                   if socket.ntohs(r.local_port & 65535) == client.getsockname()[1]
                   and socket.ntohs(r.remote_port & 65535) == server.getsockname()[1]
                   and r.local_addr == int.from_bytes(socket.inet_aton('127.0.0.1'), 'little')
                   and r.remote_addr == r.local_addr and r.state == 5)
        enabled = c.c_ubyte()
        status = get(c.byref(row), 1, c.byref(enabled), 0, 1, None, 0, 0, None, 0, 0)
        result['initial_get_status'] = status
        if status:
            return result
        if not enabled.value:
            enabled.value = 1
            status = set_stats(c.byref(row), 1, c.byref(enabled), 0, 1, 0)
            result['enable_status'] = status
            if status:
                return result
            changed = True

        def sample():
            observed = Data()
            active = c.c_ubyte()
            status = get(c.byref(row), 1, c.byref(active), 0, 1, None, 0, 0,
                         c.byref(observed), 0, c.sizeof(observed))
            # Successful calls can still contain undefined ROD fields if disabled.
            return observed.bytes_out if status == 0 and active.value else None

        before = sample()
        if before is None:
            result['collection_not_confirmed'] = True
            return result
        payload = b'x' * 16384
        started = time.monotonic()
        client.sendall(payload)
        received = bytearray()
        while len(received) < len(payload):
            chunk = server.recv(len(payload) - len(received))
            assert chunk
            received.extend(chunk)
        assert bytes(received) == payload
        deadline = time.monotonic() + 2
        after = sample()
        while (after is None or after - before < len(payload)) and time.monotonic() < deadline:
            time.sleep(.02)
            after = sample()
        result.update(payload_bytes=len(payload), elapsed_seconds=round(time.monotonic()-started, 6),
                      tcp_data_bytes_out_delta=after-before if after is not None and after >= before else None,
                      counter_verified=after is not None and after-before >= len(payload))
        return result
    finally:
        if changed:
            disabled = c.c_ubyte(0)
            result['restore_status'] = set_stats(c.byref(row), 1, c.byref(disabled), 0, 1, 0)
        client.close()
        if server:
            server.close()
        listener.close()
        result['owned_sockets_closed'] = True


if __name__ == '__main__':
    print(json.dumps(main()), flush=True)
