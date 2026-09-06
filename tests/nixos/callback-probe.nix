# Probes the real callback listener for rejection of missing or malformed authentication.
{ pkgs }:
pkgs.writeText "callback-probe.py" ''
  import base64
  import os
  import socket
  import sys

  mode = sys.argv[1]
  connection = socket.create_connection(("gateway", 7443), timeout=5)
  key = base64.b64encode(os.urandom(16)).decode()
  connection.sendall(
      (
          "GET /callback HTTP/1.1\r\n"
          "Host: gateway\r\n"
          "Upgrade: websocket\r\n"
          "Connection: Upgrade\r\n"
          f"Sec-WebSocket-Key: {key}\r\n"
          "Sec-WebSocket-Version: 13\r\n"
          "Sec-WebSocket-Protocol: telchar-nomad-transfer-v1\r\n\r\n"
      ).encode()
  )
  response = connection.recv(4096)
  if b" 101 " not in response:
      raise SystemExit(f"WebSocket upgrade rejected: {response!r}")

  if mode == "invalid":
      payload = b"not-a-telchar-authentication-frame"
      mask = os.urandom(4)
      header = bytes([0x82, 0x80 | len(payload)]) + mask
      masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
      connection.sendall(header + masked)
  elif mode != "missing":
      raise SystemExit(f"unknown mode: {mode}")

  connection.settimeout(15)
  try:
      data = connection.recv(1)
  except ConnectionResetError:
      data = b""
  if data:
      raise SystemExit(f"unauthenticated callback remained open: {data!r}")
''
