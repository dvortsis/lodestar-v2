terraform {
  required_version = ">= 1.7.0"

  required_providers {
    linode = {
      source  = "linode/linode"
      version = "~> 4.7.0"
    }
  }
}

provider "linode" {
  # Uses the LINODE_TOKEN environment variable.
  # Never paste tokens into .tf files, terraform.tfvars, or chat.
}
