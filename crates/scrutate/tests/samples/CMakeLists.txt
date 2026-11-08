# CMake sample
cmake_minimum_required(VERSION 3.10)
project(demo)
# Adds a demo target.
function(add_demo name)
  add_executable(${name} ${name}.cpp)
endfunction()
macro(setup_flags)
  set(CMAKE_CXX_STANDARD 17)
endmacro()
add_demo(hello)
