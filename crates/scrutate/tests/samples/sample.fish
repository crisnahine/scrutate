# fish sample
# Greets.
function greet
    echo hi $argv
end
function build --description "Builds"
    make all
    greet world
end
