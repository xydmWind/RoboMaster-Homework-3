#!/usr/bin/env python3
"""Exercise the two serial interfaces of the classroom bridge."""

import argparse
import queue
import threading
import time


EXTENSION = 1
FD_LENGTHS = (*range(9), 12, 16, 20, 24, 32, 48, 64)


class SerialLines:
    def __init__(self, port):
        self.port = port
        self.buffer = bytearray()

    def read_line(self, deadline):
        while True:
            end = self.buffer.find(b"\n")
            if end >= 0:
                if end > 192:
                    raise ValueError("response exceeds 192 bytes")
                raw = bytes(self.buffer[:end])
                del self.buffer[:end + 1]
                return raw.decode("ascii").rstrip("\r")
            if len(self.buffer) > 192:
                raise ValueError("response exceeds 192 bytes")
            if time.monotonic() >= deadline:
                raise TimeoutError("no complete response, check the control port and firmware")
            self.buffer.extend(self.port.read(max(1, min(64, self.port.inWaiting()))))


def transmit(port, command):
    data = command.encode("ascii")
    if len(data) > 192 or b"\n" in data or b"\r" in data:
        raise ValueError("send one command of at most 192 bytes")
    port.write(data + b"\n")
    port.flush()


def request(port, lines, command):
    transmit(port, command)
    deadline = time.monotonic() + 2
    while True:
        reply = lines.read_line(deadline)
        if reply.startswith("CAN FRAME "):
            print(reply, flush=True)
            continue
        return reply


def monitor(port, lines):
    replies = queue.Queue(maxsize=8)
    stop = threading.Event()

    def receive():
        try:
            while not stop.is_set():
                try:
                    line = lines.read_line(time.monotonic() + 0.2)
                except TimeoutError:
                    continue
                if line.startswith("CAN FRAME "):
                    print(line, flush=True)
                else:
                    replies.put_nowait(line)
        except Exception as error:
            stop.set()
            try:
                replies.put_nowait(error)
            except queue.Full:
                pass

    def send_one(command):
        if stop.is_set():
            raise OSError("serial reader stopped")
        transmit(port, command)
        try:
            value = replies.get(timeout=2)
        except queue.Empty as error:
            raise TimeoutError("command timed out, reopen monitor to start a new session") from error
        if isinstance(value, Exception):
            raise value
        print(value, flush=True)
        return value

    reader = threading.Thread(target=receive, name="can-monitor", daemon=True)
    reader.start()
    try:
        if send_one("CAN STREAM ON") != "OK STREAM ON":
            raise ValueError("firmware did not enable streaming")
        print("enter CAN TX 123 4 11223344, PING, CAN STATS. quit exits.")
        while not stop.is_set():
            try:
                command = input().strip()
            except (EOFError, KeyboardInterrupt):
                break
            if command.lower() in ("quit", "exit"):
                break
            if command:
                send_one(command)
        if not stop.is_set():
            send_one("CAN STREAM OFF")
    finally:
        stop.set()
        reader.join(timeout=1)


def hex_payload(value, maximum=8):
    if value == "-":
        return b""
    if not value or len(value) % 2 or any(c not in "0123456789abcdefABCDEF" for c in value):
        raise ValueError("payload must contain complete hex bytes, or '-' for zero bytes")
    data = bytes.fromhex(value)
    if len(data) > maximum:
        raise ValueError(f"payload exceeds {maximum} bytes")
    return data


def checked_id(value):
    if not value or any(c not in "0123456789abcdefABCDEF" for c in value):
        raise ValueError("ID must be hexadecimal without 0x")
    raw = int(value, 16)
    if not 0 <= raw <= 0x7FF:
        raise ValueError("standard CAN ID must be 000..7FF")
    return f"{raw:03X}"


def command_for(args):
    if args.command == "can-send":
        data = hex_payload(args.data)
        # 所有版本保留这个基础写法。
        return f"CAN TX {checked_id(args.id)} {data.hex().upper() or '-'}"
    if args.command == "canfd-send":
        data = bytes(range(args.length)) if args.data is None else hex_payload(args.data, 64)
        if len(data) != args.length:
            raise ValueError("--length must equal the actual payload byte count")
        return f"CANFD TX {checked_id(args.id)} {args.length} {args.brs} {data.hex().upper() or '-'}"
    return {"ping": "PING", "swd": "SWD ID", "can-recv": "CAN RX", "canfd-recv": "CANFD RX", "stats": "CAN STATS"}[args.command]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("port")
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ("ping", "swd", "can-recv", "uart-loopback"):
        commands.add_parser(name)
    send = commands.add_parser("can-send")
    send.add_argument("id")
    send.add_argument("data")
    if EXTENSION == 1:
        commands.add_parser("monitor")
        commands.add_parser("stats")
    if EXTENSION == 2:
        send_fd = commands.add_parser("canfd-send")
        send_fd.add_argument("id")
        send_fd.add_argument("--length", type=int, choices=FD_LENGTHS, required=True)
        send_fd.add_argument("--brs", type=int, choices=(0, 1), default=1)
        send_fd.add_argument("--data")
        commands.add_parser("canfd-recv")
    args = parser.parse_args()
    try:
        import serial
    except ImportError:
        parser.exit(2, "Install pyserial 3.5 in the Python environment first.\n")
    try:
        with serial.Serial(args.port, 115200, timeout=0.1, write_timeout=2) as port:
            port.reset_input_buffer()
            if args.command == "uart-loopback":
                port.timeout = 2
                expected = b"HerKules UART\n"
                port.write(expected)
                got = port.read(len(expected))
                if got != expected:
                    raise ValueError(f"Expected {expected!r}, received {got!r}")
                print("OK UART loopback")
                return
            lines = SerialLines(port)
            if args.command == "monitor":
                monitor(port, lines)
                return
            reply = request(port, lines, command_for(args))
            print(reply)
            if not (reply.startswith("OK ") or reply == "EMPTY"):
                parser.exit(1)
    except (OSError, ValueError, TimeoutError, serial.SerialException) as error:
        parser.exit(1, f"{error}\n")
    except KeyboardInterrupt:
        parser.exit(130, "Interrupted\n")


if __name__ == "__main__":
    main()
