import os
import sys
import textwrap

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

FILES = {
    "app/auth.py": '''
        """Authentication helpers."""
        import time


        class TokenStore:
            """Keeps OAuth tokens."""

            def refresh_token(self, force=False):
                """Refresh an expired access token."""
                if self._expired() or force:
                    return self._post("/token")

            def _expired(self):
                return time.time() > self.expiry

            def _post(self, url):
                return url


        def calculate_token_expiry(expires_in):
            """Work out when a token expires."""
            return time.time() + expires_in
    ''',
    "app/server.py": '''
        from .auth import TokenStore


        def send_ping(session):
            """Liveness check: ping the peer."""
            return session.send_request("ping")


        def send_request(session, payload):
            return session.write(payload)


        class Server:
            def add_tool(self, tool):
                """Register a new tool with the server."""
                self.tools[tool.name] = tool

            def remove_tool(self, name):
                """Delete a tool from the server by name."""
                del self.tools[name]

            def handle(self, request):
                store = TokenStore()
                store.refresh_token()
                return send_request(self, request)
    ''',
    "app/util.js": '''
        /**
         * Format a user name for display.
         */
        function formatName(first, last) {
          return first + " " + last;
        }
    ''',
    "tests/test_auth.py": '''
        from app.auth import calculate_token_expiry


        def test_expiry():
            """refresh token expired expires"""
            assert calculate_token_expiry(1)
    ''',
    "README.md": "# Demo\n\nA tiny demo app with a token store and a server.\n",
}


def write_repo(root, files=FILES):
    for rel, body in files.items():
        p = os.path.join(root, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w") as f:
            f.write(textwrap.dedent(body).lstrip("\n"))


@pytest.fixture()
def repo(tmp_path):
    write_repo(str(tmp_path))
    return str(tmp_path)
