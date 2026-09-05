"""A dependency-free RESP2 client, just enough for the dev fixture scripts.

The app speaks RESP3 through `fred`; these scripts only need to write data, so
they use RESP2 (which every Redis >= 2.0 answers) over a plain socket. That
keeps `scripts/` runnable with a bare `python3`, with no virtualenv and no pip.
"""

import socket


class RedisError(Exception):
    """A `-ERR ...` reply, carrying the server's message verbatim."""


class Resp:
    def __init__(self, host="127.0.0.1", port=6379, username=None, password=None, db=0):
        self.sock = socket.create_connection((host, port), timeout=10)
        self.buf = b""
        if password:
            self.call("AUTH", *( (username, password) if username else (password,) ))
        if db:
            self.call("SELECT", db)

    # -- wire ---------------------------------------------------------------

    @staticmethod
    def _encode(args):
        out = [b"*%d\r\n" % len(args)]
        for a in args:
            if isinstance(a, str):
                a = a.encode()
            elif not isinstance(a, bytes):
                a = str(a).encode()
            out.append(b"$%d\r\n%s\r\n" % (len(a), a))
        return b"".join(out)

    def _readline(self):
        while b"\r\n" not in self.buf:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("server closed the connection")
            self.buf += chunk
        line, self.buf = self.buf.split(b"\r\n", 1)
        return line

    def _readexact(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("server closed the connection")
            self.buf += chunk
        data, self.buf = self.buf[:n], self.buf[n:]
        return data

    def _reply(self):
        line = self._readline()
        tag, rest = line[:1], line[1:]
        if tag == b"+":
            return rest.decode()
        if tag == b"-":
            raise RedisError(rest.decode())
        if tag == b":":
            return int(rest)
        if tag == b"$":
            n = int(rest)
            if n == -1:
                return None
            data = self._readexact(n + 2)[:-2]
            return data
        if tag == b"*":
            n = int(rest)
            if n == -1:
                return None
            return [self._reply() for _ in range(n)]
        raise RedisError("unparsable reply: %r" % line)

    # -- api ----------------------------------------------------------------

    def call(self, *args):
        self.sock.sendall(self._encode(args))
        return self._reply()

    def pipeline(self, commands):
        """Send every command, then drain every reply. Errors are returned, not
        raised, so one bad command in a batch does not lose the rest."""
        if not commands:
            return []
        self.sock.sendall(b"".join(self._encode(c) for c in commands))
        out = []
        for _ in commands:
            try:
                out.append(self._reply())
            except RedisError as e:
                out.append(e)
        return out

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


def add_target_args(parser):
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=6379)
    parser.add_argument("--username", default=None)
    parser.add_argument("--password", default=None)
    parser.add_argument("--db", type=int, default=0)


def connect(args):
    return Resp(args.host, args.port, args.username, args.password, args.db)
