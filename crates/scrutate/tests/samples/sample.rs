//! Demo crate.
use std::fmt;

/// A point.
pub struct Point { pub x: i32 }

pub trait Shape { fn area(&self) -> f64; }

impl Shape for Point {
    fn area(&self) -> f64 { helper(self.x) as f64 }
}

pub enum Color { Red, Green }

pub mod inner {
    pub fn nested() {}
}

/// Greets.
pub fn greet(name: &str) -> String { format!("hi {name}") }

fn helper(x: i32) -> i32 { x * 2 }
pub type Id = u64;
