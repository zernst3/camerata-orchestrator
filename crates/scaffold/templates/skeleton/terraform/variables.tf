variable "project" {
  type        = string
  default     = "{{APP_NAME_SNAKE}}"
  description = "Project name prefix used in all resource names."
}

variable "environment" {
  type        = string
  default     = "prod"
  description = "Deployment environment. Single-app skeleton, one env by default."
}

variable "location" {
  type        = string
  default     = "eastus"
  description = "Azure region for new resources."
}

variable "container_image" {
  type        = string
  default     = "ghcr.io/CHANGE_ME/{{APP_NAME_SNAKE}}:latest"
  description = "Fully-qualified container image reference. The CI workflow builds + pushes this once a registry stage is wired up (not yet part of ci.yml — build + test only, no deploy)."
}

variable "ghcr_username" {
  type        = string
  default     = ""
  description = "GHCR username, if pulling a private image."
}

variable "ghcr_pull_token" {
  type        = string
  sensitive   = true
  default     = ""
  description = "GHCR pull token (PAT with read:packages), if pulling a private image. No default: supply via TF_VAR_ghcr_pull_token or a .tfvars file, never commit a value here."
}

variable "container_cpu" {
  type        = number
  default     = 0.25
  description = "vCPU per replica. 0.25 is the smallest Consumption-plan size."
}

variable "container_memory" {
  type        = string
  default     = "0.5Gi"
  description = "Memory per replica. Must pair with container_cpu per Container Apps allowed combos."
}

variable "container_target_port" {
  type        = number
  default     = 8080
  description = "Port the app listens on inside the container (matches src/main.rs's PORT env var read)."
}

variable "log_retention_days" {
  type        = number
  default     = 30
  description = "Log Analytics retention."
}
