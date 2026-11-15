"""Minimal stdio client for `egui-mcp` (the MCP server that drives an egui app through its inspection port).

Only the Python standard library. One `Client` owns one `egui-mcp` child process; `call(tool, **args)` returns
the parsed JSON of the first text item (and the image items are ignored; use `screenshot(save_path=...)`).
"""
import json
import subprocess


class ToolError(RuntimeError):
    pass


class Client:
    def __init__(self, port, timeout=30):
        self.proc = subprocess.Popen(
            ["egui-mcp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True
        )
        self._id = 0
        self._rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {},
                                  "clientInfo": {"name": "readme-media", "version": "1"}})
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        self.call("attach", host="127.0.0.1", port=port, timeout_secs=timeout)

    def _send(self, msg):
        self.proc.stdin.write(json.dumps(msg) + "\n")
        self.proc.stdin.flush()

    def _rpc(self, method, params):
        self._id += 1
        self._send({"jsonrpc": "2.0", "id": self._id, "method": method, "params": params})
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise ToolError("egui-mcp closed")
            msg = json.loads(line)
            if msg.get("id") == self._id:
                if "error" in msg:
                    raise ToolError(str(msg["error"]))
                return msg["result"]

    def call(self, tool, **args):
        res = self._rpc("tools/call", {"name": tool, "arguments": args})
        texts = [c["text"] for c in res.get("content", []) if c.get("type") == "text"]
        text = texts[0] if texts else ""
        if res.get("isError"):
            raise ToolError(f"{tool}: {text}")
        try:
            return json.loads(text)
        except ValueError:
            return text

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()
