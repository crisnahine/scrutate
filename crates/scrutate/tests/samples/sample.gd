extends CharacterBody2D
## A player that moves and scores.

signal scored(points)

enum State { IDLE, RUNNING }

const MAX_SPEED = 400

@export var speed := 100


func _init():
	speed = 50


## Moves the player by its speed.
func move(delta):
	position.x += speed * delta
	clamp_speed()
	scored.emit(1)


static func clamp_speed():
	return min(MAX_SPEED, 1)


class Inventory:
	var items = []

	## Adds an item.
	func add(item):
		items.append(item)
