(* Geometry helpers *)
type point = { x : float; y : float }

let distance p q =
  let dx = p.x -. q.x and dy = p.y -. q.y in
  sqrt ((dx *. dx) +. (dy *. dy))

module Shape = struct
  let area_of_square side = side *. side
end

let main () = print_float (distance { x = 0.; y = 0. } { x = 3.; y = 4. })
