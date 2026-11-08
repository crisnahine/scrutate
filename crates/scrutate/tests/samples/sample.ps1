# PowerShell sample
# Greets.
function Get-Greeting {
    param([string]$Name)
    Write-Output "Hi $Name"
}
class Greeter {
    [string]$Name
    [string] Greet() { return "Hi" }
}
Get-Greeting -Name x
