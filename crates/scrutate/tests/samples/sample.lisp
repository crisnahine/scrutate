;;; common lisp sample
(defun greet (name)
  "Greet NAME."
  (format nil "hi ~a" name))
(defmacro twice (x) `(* 2 ,x))
(defclass point () ((x :initarg :x)))
(defvar *version* "1.0")
(greet "x")
