;; scheme sample
(define (greet name)
  (string-append "hi " name))
(define version "1.0")
(define-syntax twice
  (syntax-rules () ((_ x) (* 2 x))))
(greet "x")
