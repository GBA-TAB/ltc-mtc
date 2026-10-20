#!/usr/bin/env python3
"""Prints the OSC messages it receives, one JSON line each, with a receive timestamp.

usage: osc-listen.py udp PORT
       osc-listen.py tcp PORT        (SLIP framing, OSC 1.1)
       osc-listen.py tcp-len PORT    (int32 length prefix, OSC 1.0)
"""
import json, socket, struct, sys, time

def s(buf, i):
    end = buf.index(b"\0", i)
    return buf[i:end].decode(), (end + 4) & ~3

def decode(buf):
    addr, i = s(buf, 0)
    tags, i = s(buf, i)
    args = []
    for t in tags[1:]:
        if t == "i": args.append(struct.unpack(">i", buf[i:i + 4])[0]); i += 4
        elif t == "f": args.append(round(struct.unpack(">f", buf[i:i + 4])[0], 6)); i += 4
        elif t == "s": v, i = s(buf, i); args.append(v)
        elif t in "TF": args.append(t == "T")
        else: raise ValueError("type " + t)
    if i != len(buf): raise ValueError("trailing bytes")
    return {"t": round(time.time(), 4), "address": addr, "tags": tags, "args": args}

def emit(packet):
    try: print(json.dumps(decode(packet)), flush=True)
    except Exception as e: print(json.dumps({"error": str(e), "hex": packet.hex()}), flush=True)

mode, port = sys.argv[1], int(sys.argv[2])
if mode == "udp":
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(("0.0.0.0", port))
    while True:
        emit(sock.recv(65536))
srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("0.0.0.0", port))
srv.listen(4)
while True:
    conn, peer = srv.accept()
    print(json.dumps({"connected": peer[0]}), flush=True)
    buf = b""
    while True:
        data = conn.recv(65536)
        if not data: break
        buf += data
        if mode == "tcp-len":
            while len(buf) >= 4 and len(buf) >= 4 + struct.unpack(">i", buf[:4])[0]:
                n = struct.unpack(">i", buf[:4])[0]
                emit(buf[4:4 + n]); buf = buf[4 + n:]
        else:  # SLIP: END=C0, ESC=DB, ESC_END=DC, ESC_ESC=DD
            while b"\xc0" in buf:
                frame, buf = buf.split(b"\xc0", 1)
                if frame:
                    emit(frame.replace(b"\xdb\xdc", b"\xc0").replace(b"\xdb\xdd", b"\xdb"))
    print(json.dumps({"disconnected": peer[0]}), flush=True)
    conn.close()
