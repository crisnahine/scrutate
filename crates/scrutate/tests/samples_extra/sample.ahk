; Greet the user with a hotkey.
Greet(name) {
    MsgBox, Hello %name%
}

Sum(a, b) {
    return a + b
}

^j::
Greet("World")
return
