package demo;

/** A greeter. */
public class Greeter {
    private final String name;
    public Greeter(String name) { this.name = name; }
    /** Greets. */
    public String greet() { return helper(name); }
    private static String helper(String s) { return "hi " + s; }
}
interface Shape { double area(); }
enum Color { RED, GREEN }
record Pt(int x) {}
