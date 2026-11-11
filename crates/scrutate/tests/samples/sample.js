// Sample JavaScript module.
import { readFile } from "fs";

/** A greeter. */
class Greeter {
  constructor(name) {
    this.name = name;
  }

  /** Say hello. */
  greet() {
    return helper(`hello ${this.name}`);
  }
}

// Format a message.
function helper(message) {
  return message.trim();
}

const arrow = (x) => x * 2;

export { Greeter, helper, arrow };

const store = { count: 0, increment() { return this.total(); }, total: () => 1 };
