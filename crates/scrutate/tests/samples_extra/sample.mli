(* A bounded queue *)
type 'a t

type error = Full | Empty

module type ORDERED = sig
  type key
  val compare : key -> key -> int
end

module Priority : sig
  type 'a q
  val push : 'a q -> int -> 'a -> unit
end

val create : int -> 'a t
val push : 'a t -> 'a -> (unit, error) result
val pop : 'a t -> ('a, error) result
