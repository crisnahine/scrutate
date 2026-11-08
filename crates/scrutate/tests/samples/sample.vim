" Vim sample
" Greets.
function! Greet(name)
  echo "hi " . a:name
endfunction
function s:helper(x) abort
  return a:x * 2
endfunction
call Greet("x")
