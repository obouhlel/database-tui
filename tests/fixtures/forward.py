"""Minimal TCP forwarder standing in for `ssh -N -L {port}:host:port` in tests.

Usage: python3 forward.py LOCAL_PORT TARGET_HOST TARGET_PORT
"""
import socket
import sys
import threading


def pipe(src, dst):
    try:
        while data := src.recv(65536):
            dst.sendall(data)
    except OSError:
        pass
    finally:
        for s in (src, dst):
            try:
                s.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass


def main():
    local_port, host, port = int(sys.argv[1]), sys.argv[2], int(sys.argv[3])
    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", local_port))
    server.listen()
    while True:
        client, _ = server.accept()
        try:
            upstream = socket.create_connection((host, port))
        except OSError as e:
            # Like ssh: report and drop this connection, keep the tunnel up.
            print(f"forward: {e}", file=sys.stderr, flush=True)
            client.close()
            continue
        threading.Thread(target=pipe, args=(client, upstream), daemon=True).start()
        threading.Thread(target=pipe, args=(upstream, client), daemon=True).start()


main()
