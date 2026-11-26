"""FTP source node: a minimal passive-mode FTP server (stdlib only) whose uploads become frames.

An industrial camera (for example a Cognex camera acting as an FTP client) uploads each inspection
image with STOR. The node turns every completed upload into one typed frame for the graph, written
straight into the engine-owned shared-memory slot (the bytes arrive over TCP, which is not zero
copy; from there on nothing is copied).

Config (node config, via `configure`):
  port            control port inside the node (default 2121)
  passiveRange    "lo-hi" data ports (default "50000-50009"); publish the same range
  user, password  the only accepted credentials (default anonymous: any user, any password)
  maxFiles        end the stream after N uploads (0 = never; useful in tests)
Environment: MVF_ADVERTISE_IP is the address put in PASV replies (the host address the camera
reaches); without it the node advertises the address the control connection arrived on.

Plain FTP sends credentials in clear text: use it only on an isolated, authorized industrial
network. Credentials are never logged.
"""
import os
import queue
import socket
import sys
import threading
import time

try:
    import resource  # Unix only; peak RSS is reported where it exists
except ImportError:  # Windows
    resource = None

from mvf_sdk import log, run_source, source_frame

_config = {"port": 2121, "passiveRange": "50000-50009", "user": None, "password": None, "maxFiles": 0}
_frames = queue.Queue()
_started = threading.Event()
_count = {"files": 0, "emitted": 0}


def _passive_ports():
    lo, hi = (int(x) for x in str(_config["passiveRange"]).split("-"))
    return lo, hi


class _Session(threading.Thread):
    def __init__(self, conn, peer):
        super().__init__(daemon=True)
        # Replies are small and request/response: without TCP_NODELAY each one can wait for the
        # client's delayed ACK (Nagle), adding ~40 ms per transfer.
        conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.conn, self.peer = conn, peer
        self.user = None
        self.authed = False
        self.data = None

    def reply(self, text):
        self.conn.sendall((text + "\r\n").encode())

    def open_passive(self):
        lo, hi = _passive_ports()
        for port in range(lo, hi + 1):
            s = socket.socket()
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                s.bind(("0.0.0.0", port))
            except OSError:
                s.close()
                continue
            s.listen(1)
            s.settimeout(15)
            return s, port
        raise OSError("no free passive port")

    def run(self):
        try:
            self.reply("220 MVF FTP source ready")
            reader = self.conn.makefile("rb")
            for raw in reader:
                line = raw.decode(errors="replace").strip()
                if not line:
                    continue
                cmd, _, arg = line.partition(" ")
                cmd = cmd.upper()
                if cmd == "USER":
                    self.user = arg
                    self.reply("331 password required")
                elif cmd == "PASS":
                    want_user, want_pass = _config["user"], _config["password"]
                    if want_user is None or (self.user == want_user and arg == want_pass):
                        self.authed = True
                        self.reply("230 logged in")
                    else:
                        self.reply("530 login incorrect")
                elif cmd == "QUIT":
                    self.reply("221 bye")
                    return
                elif not self.authed:
                    self.reply("530 log in first")
                elif cmd in ("SYST",):
                    self.reply("215 UNIX Type: L8")
                elif cmd in ("TYPE", "MODE", "STRU"):
                    self.reply("200 ok")
                elif cmd in ("PWD",):
                    self.reply('257 "/"')
                elif cmd in ("CWD", "MKD"):
                    self.reply("250 ok")
                elif cmd == "NOOP":
                    self.reply("200 ok")
                elif cmd == "PASV":
                    if self.data is not None:
                        self.data.close()
                    self.data, port = self.open_passive()
                    ip = os.environ.get("MVF_ADVERTISE_IP") or self.conn.getsockname()[0]
                    h = ip.split(".")
                    self.reply("227 Entering Passive Mode (%s,%s,%s,%s,%d,%d)"
                               % (h[0], h[1], h[2], h[3], port // 256, port % 256))
                elif cmd == "STOR":
                    if self.data is None:
                        self.reply("425 use PASV first")
                        continue
                    self.reply("150 ok to send data")
                    try:
                        dconn, _ = self.data.accept()
                    except OSError:
                        self.reply("425 no data connection")
                        continue
                    chunks = []
                    while True:
                        chunk = dconn.recv(1 << 16)
                        if not chunk:
                            break
                        chunks.append(chunk)
                    dconn.close()
                    self.data.close()
                    self.data = None
                    payload = b"".join(chunks)
                    _count["files"] += 1
                    _frames.put((self.user or "ftp", payload, arg))
                    self.reply("226 transfer complete")
                else:
                    self.reply("502 not implemented")
        except OSError:
            pass
        finally:
            self.conn.close()


def _serve():
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("0.0.0.0", int(_config["port"])))
    s.listen(16)
    log("ftp: listening on control port {} passive {}".format(_config["port"], _config["passiveRange"]))
    _started.set()
    while True:
        conn, peer = s.accept()
        _Session(conn, peer).start()


def _start():
    if not _started.is_set():
        threading.Thread(target=_serve, daemon=True).start()
        _started.wait(10)


def on_configure(config):
    for key in ("port", "passiveRange", "user", "password", "maxFiles"):
        if key in config:
            _config[key] = config[key]
    _start()


def produce():
    _start()
    limit = int(_config["maxFiles"] or 0)
    if limit and _count["emitted"] >= limit:
        rss_kib = "n/a"
        if resource is not None:
            usage = resource.getrusage(resource.RUSAGE_SELF)
            rss_kib = usage.ru_maxrss // (1024 if sys.platform == "darwin" else 1)
        log("ftp: done files={} peak_rss_kib={} cpu_s={:.3f}".format(
            _count["files"], rss_kib, time.process_time()))
        return None
    camera, payload, _name = _frames.get()
    _count["emitted"] += 1
    return source_frame(payload, camera_id=camera, sequence=_count["emitted"])


run_source("py.ftp-source", produce, on_configure=on_configure)
