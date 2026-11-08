module Demo exposing (greet)

-- A point.
type alias Point = { x : Int }

type Color = Red | Green

{-| Greets. -}
greet : String -> String
greet name =
    "hi " ++ name
