-module(demo).
-export([greet/1]).
-record(user, {name, age}).
-type name() :: string().

%% Greets.
greet(Name) ->
    io:format("hi ~s~n", [Name]).

helper(X) -> X * 2.
