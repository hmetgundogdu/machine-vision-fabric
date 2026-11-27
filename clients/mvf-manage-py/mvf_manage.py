#!/usr/bin/env python3
"""A minimal, independent client for the MVF management API v1 (#33).

Standard library only. It speaks the protocol in protocol/management-v1.md: newline-delimited
JSON over the runtime's local endpoint. That is a Unix socket on Linux and macOS, and a named
pipe on Windows. The Rust CLI (`mvf-rs manage`) is the reference client; this one shows that
nothing in the API is Rust-specific.

    python3 mvf_manage.py list
    python3 mvf_manage.py --endpoint PATH status [--json]
    python3 mvf_manage.py --endpoint PATH watch [--from SEQ] [--until-stopped] [--json]
    python3 mvf_manage.py --endpoint PATH command NAME --id ID [--args JSON]

Without --endpoint it uses the single runtime registered in the per-user runtime directory.
Through an `mvf-remote` relay, over mutual TLS 1.3:

    python3 mvf_manage.py --remote HOST:PORT --ca ca.pem --cert me.pem --key me.key status
"""

import argparse
import glob
import json
import os
import socket
import ssl
import sys

API = 1


def runtime_dir():
    if os.environ.get("MVF_RUNTIME_DIR"):
        return os.environ["MVF_RUNTIME_DIR"]
    if os.name == "nt":
        return os.path.join(os.environ["LOCALAPPDATA"], "mvf", "run")
    if os.environ.get("XDG_RUNTIME_DIR"):
        return os.path.join(os.environ["XDG_RUNTIME_DIR"], "mvf")
    return os.path.join(os.path.expanduser("~"), ".mvf", "run")


def registered():
    out = []
    for path in sorted(glob.glob(os.path.join(runtime_dir(), "*.json"))):
        try:
            with open(path, encoding="utf-8") as f:
                out.append(json.load(f))
        except (OSError, ValueError):
            pass
    return out


class Connection:
    """One connection: line-oriented JSON both ways."""

    def __init__(self, endpoint, remote=None):
        if remote is not None:
            host, port = remote["address"].rsplit(":", 1)
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            context.minimum_version = ssl.TLSVersion.TLSv1_3
            context.load_verify_locations(remote["ca"])
            context.load_cert_chain(remote["cert"], remote["key"])
            raw = socket.create_connection((host, int(port)))
            self._sock = context.wrap_socket(raw, server_hostname=host)
            self._read = self._sock.recv
            self._write = self._sock.sendall
        elif os.name == "nt":
            self._file = open(endpoint, "r+b", buffering=0)
            self._read = self._file.read
            self._write = self._file.write
        else:
            self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self._sock.connect(endpoint)
            self._read = self._sock.recv
            self._write = self._sock.sendall
        self._buf = b""

    def send(self, obj):
        self._write((json.dumps(obj) + "\n").encode())

    def receive(self):
        while b"\n" not in self._buf:
            chunk = self._read(65536)
            if not chunk:
                return None
            self._buf += chunk
        line, self._buf = self._buf.split(b"\n", 1)
        return json.loads(line)

    def request(self, obj):
        self.send(obj)
        return self.receive()


def connect(endpoint, remote=None):
    conn = Connection(endpoint, remote)
    reply = conn.request({"op": "hello", "api": [API], "client": "mvf-manage-py"})
    if not reply or reply.get("op") != "hello":
        raise SystemExit("refused: %s" % json.dumps(reply))
    return conn


def pick_endpoint(args):
    if args.endpoint:
        return args.endpoint
    found = registered()
    if len(found) != 1:
        raise SystemExit("%d runtimes registered; pass --endpoint" % len(found))
    return found[0]["endpoint"]


def show_status(snapshot):
    print("%s: %s (cycle %s)" % (snapshot["runtime"]["pipeline"], snapshot["status"], snapshot["cycle"]))
    for node in snapshot["topology"]["nodes"]:
        view = snapshot["nodes"].get(node["id"], {})
        lat = view.get("latencyUs", {})
        print("  %-16s %-10s gen %-3s executed %-8s p50 %8.1f us  p99 %8.1f us" % (
            node["id"], node["backend"], view.get("generation", "-"), view.get("executed", 0),
            lat.get("p50", 0.0), lat.get("p99", 0.0)))


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--endpoint")
    parser.add_argument("--remote", help="an mvf-remote relay, HOST:PORT")
    parser.add_argument("--ca")
    parser.add_argument("--cert")
    parser.add_argument("--key")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("list")
    status = sub.add_parser("status")
    status.add_argument("--json", action="store_true")
    watch = sub.add_parser("watch")
    watch.add_argument("--from", dest="start", type=int)
    watch.add_argument("--until-stopped", action="store_true")
    watch.add_argument("--json", action="store_true")
    command = sub.add_parser("command")
    command.add_argument("name")
    command.add_argument("--id", required=True)
    command.add_argument("--args", default="{}")
    command.add_argument("--deadline-ms", type=int)
    args = parser.parse_args()

    if args.command == "list":
        for r in registered():
            print("%-8s %-24s %s" % (r["pid"], r["pipeline"], r["endpoint"]))
        return 0

    if args.remote:
        conn = connect(None, {"address": args.remote, "ca": args.ca, "cert": args.cert, "key": args.key})
    else:
        conn = connect(pick_endpoint(args))
    if args.command == "command":
        request = {"op": "command", "id": args.id, "command": args.name, "args": json.loads(args.args)}
        if args.deadline_ms is not None:
            request["deadlineMs"] = args.deadline_ms
        reply = conn.request(request)
        print(json.dumps(reply))
        return 0 if reply.get("status") == "applied" else 1
    if args.command == "status":
        reply = conn.request({"op": "snapshot"})
        if args.json:
            print(json.dumps(reply["snapshot"]))
        else:
            show_status(reply["snapshot"])
        return 0

    request = {"op": "subscribe"}
    if args.start is not None:
        request["from"] = args.start
    conn.send(request)
    while True:
        msg = conn.receive()
        if msg is None:
            return 0
        if args.json:
            print(json.dumps(msg), flush=True)
        elif msg["op"] == "entry":
            entry = msg["entry"]
            if entry["kind"] == "log":
                print("[%s] %s %s: %s" % (msg["seq"], entry["node"], entry["level"], entry["message"]), flush=True)
            elif entry["kind"] == "delta":
                rates = ", ".join("%s %.0f/s" % (k, v["window"]["perSecond"]) for k, v in entry["nodes"].items())
                print("[%s] cycle %s: %s" % (msg["seq"], entry["cycle"], rates), flush=True)
            else:
                print("[%s] %s" % (msg["seq"], json.dumps(entry)), flush=True)
        elif msg["op"] in ("gap", "snapshot"):
            print("%s %s" % (msg["op"], json.dumps({k: v for k, v in msg.items() if k != "snapshot"})), flush=True)
        if args.until_stopped and msg.get("op") == "entry" and msg["entry"].get("status") == "stopped":
            return 0


if __name__ == "__main__":
    sys.exit(main())
