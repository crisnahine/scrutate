# Terraform sample
resource "aws_instance" "web" {
  ami           = "ami-123"
  instance_type = "t3.micro"
}
variable "region" {
  default = "us-east-1"
}
module "vpc" {
  source = "./vpc"
  cidr   = cidrsubnet("10.0.0.0/8", 8, 1)
}
