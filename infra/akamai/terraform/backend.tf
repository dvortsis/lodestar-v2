# Terraform state in S3 (OP-2a; docs/plan/2026-11-08-cloud-operator.md §3-§4).
# A partial configuration: the bucket, key and region come only from
# -backend-config at init, never from this file, e.g.
#   terraform init -backend-config="bucket=<STATE_BUCKET>" \
#     -backend-config="key=terraform/lodestar.tfstate" -backend-config="region=us-east-1"
# use_lockfile is Terraform 1.10's native S3 locking: a <key>.tflock object,
# no DynamoDB table. The state bucket is SSE-S3 and versioned
# (infra/aws/fleet-control).
terraform {
  backend "s3" {
    use_lockfile = true
    encrypt      = true
  }
}
