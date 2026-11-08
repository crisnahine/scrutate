;;; demo.el --- Demo -*- lexical-binding: t -*-
(defun demo-greet (name)
  "Greet NAME."
  (message "hi %s" name))
(defmacro demo-twice (x)
  `(* 2 ,x))
(defvar demo-version "1.0")
(demo-greet "x")
