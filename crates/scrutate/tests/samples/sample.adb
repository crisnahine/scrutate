-- Ada sample
with Ada.Text_IO;
package body Demo is
   type Point is record
      X : Integer;
   end record;
   -- Greets.
   procedure Greet (Name : String) is
   begin
      Ada.Text_IO.Put_Line ("hi " & Name);
   end Greet;
   function Add (A, B : Integer) return Integer is
   begin
      return A + B;
   end Add;
end Demo;
