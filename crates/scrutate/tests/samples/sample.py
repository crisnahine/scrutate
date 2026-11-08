"""Demo module."""
import os


class Greeter:
    """Greets."""

    def __init__(self, name):
        self.name = name

    def greet(self):
        """Say hi."""
        return helper(self.name)


def helper(s):
    return os.path.join("hi", s)
