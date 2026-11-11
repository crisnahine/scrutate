// Package demo greets.
package demo

import "fmt"

// Point is a point.
type Point struct{ X, Y int }

// Greet says hi.
func Greet(name string) string { return fmt.Sprintf("hi %s", name) }

// Len returns the length.
func (p Point) Len() int { return abs(p.X) }

func abs(x int) int { return x }

const MinLen, MaxLen = 1, 9
