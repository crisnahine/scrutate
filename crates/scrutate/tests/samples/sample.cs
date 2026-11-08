using System;
namespace Demo {
    /// <summary>Greets people.</summary>
    public class Greeter {
        public string Name { get; set; }
        public Greeter(string name) { Name = name; }
        public void Greet() { Console.WriteLine(Format(Name)); }
        private static string Format(string n) => $"Hi {n}";
    }
    public interface IShape { double Area(); }
    public struct Pt { public int X; }
    public enum Color { Red, Green }
    public record Person(string First);
}
