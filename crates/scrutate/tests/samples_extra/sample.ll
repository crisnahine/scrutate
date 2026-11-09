; ModuleID = 'sample'
define i32 @add(i32 %a, i32 %b) {
entry:
  %sum = add i32 %a, %b
  ret i32 %sum
}

define i32 @main() {
entry:
  %r = call i32 @add(i32 2, i32 3)
  ret i32 %r
}
