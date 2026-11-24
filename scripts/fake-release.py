# A fake Kerno release at https://github.com/EvanPluchart/kerno/releases, for the Windows update
# check of scripts/windows-update-check.sh on a CI runner only: that script points github.com at
# this machine (the hosts file) and trusts this server's certificate for the step, so Kerno's
# own update code runs unchanged against it.
#   python fake-release.py <folder> <version> <cert.pem> <key.pem>
# HEAD or GET of .../releases/latest answers 302 to .../releases/tag/v<version>, as GitHub does;
# .../releases/download/v<version>/<name> serves <folder>/<name>, read at each request; anything
# else is a 404. Each request is logged on stderr.
import http.server
import os
import ssl
import sys

folder, version, cert, key = sys.argv[1:5]
PREFIX = "/EvanPluchart/kerno/releases"


class Release(http.server.BaseHTTPRequestHandler):
    def do_HEAD(self):
        self.answer(False)

    def do_GET(self):
        self.answer(True)

    def answer(self, body):
        if self.path == PREFIX + "/latest":
            self.send_response(302)
            self.send_header("Location", f"https://github.com{PREFIX}/tag/v{version}")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        download = f"{PREFIX}/download/v{version}/"
        name = self.path[len(download):] if self.path.startswith(download) else ""
        path = os.path.join(folder, name)
        if name and "/" not in name and "\\" not in name and os.path.isfile(path):
            with open(path, "rb") as file:
                data = file.read()
            self.send_response(200)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            if body:
                self.wfile.write(data)
            return
        self.send_response(404)
        self.send_header("Content-Length", "0")
        self.end_headers()


context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(cert, key)
server = http.server.ThreadingHTTPServer(("127.0.0.1", 443), Release)
server.socket = context.wrap_socket(server.socket, server_side=True)
print(f"serving Kerno {version} from {folder}", file=sys.stderr, flush=True)
server.serve_forever()
