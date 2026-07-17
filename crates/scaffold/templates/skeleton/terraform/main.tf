###############################################################################
# Minimal Azure compute for {{APP_NAME}} — a scale-to-zero Container App running
# the fullstack binary (Axum + Dioxus SSR + server functions, no database: this
# skeleton is DB-on-demand). Modeled on the itinerary-app / budget-tracker
# reference apps' Container Apps terraform, trimmed to compute only.
#
# No database, no Key Vault, no managed identity here on purpose — add those in a
# later change only when the app actually needs persistence or secrets, following
# the same pattern the reference apps use (Key Vault secret reference ->
# Container App secret, resolved by a user-assigned managed identity).
###############################################################################

terraform {
  required_version = ">= 1.9.0"

  required_providers {
    azurerm = {
      source  = "hashicorp/azurerm"
      version = "~> 4.0"
    }
  }
}

provider "azurerm" {
  features {}
}

locals {
  name_prefix = "${var.project}-${var.environment}"
  tags = {
    project     = var.project
    environment = var.environment
    managed_by  = "terraform"
  }
}

resource "azurerm_resource_group" "main" {
  name     = "${local.name_prefix}-rg"
  location = var.location
  tags     = local.tags
}

# Free-tier-friendly log destination for the Container Apps environment.
resource "azurerm_log_analytics_workspace" "main" {
  name                = "${local.name_prefix}-law"
  resource_group_name = azurerm_resource_group.main.name
  location            = azurerm_resource_group.main.location
  sku                 = "PerGB2018"
  retention_in_days   = var.log_retention_days
  tags                = local.tags
}

resource "azurerm_container_app_environment" "main" {
  name                       = "${local.name_prefix}-cae"
  resource_group_name        = azurerm_resource_group.main.name
  location                   = azurerm_resource_group.main.location
  log_analytics_workspace_id = azurerm_log_analytics_workspace.main.id
  tags                       = local.tags
}

resource "azurerm_container_app" "main" {
  name                         = "${local.name_prefix}-app"
  resource_group_name         = azurerm_resource_group.main.name
  container_app_environment_id = azurerm_container_app_environment.main.id
  revision_mode                = "Single"
  tags                         = local.tags

  # GHCR pull credential — only wired when a token is supplied (public images or a
  # first local `az containerapp` deploy can skip this).
  dynamic "registry" {
    for_each = var.ghcr_pull_token != "" ? [1] : []
    content {
      server               = "ghcr.io"
      username             = var.ghcr_username
      password_secret_name = "ghcr-pull-token"
    }
  }

  dynamic "secret" {
    for_each = var.ghcr_pull_token != "" ? [1] : []
    content {
      name  = "ghcr-pull-token"
      value = var.ghcr_pull_token
    }
  }

  ingress {
    external_enabled = true
    target_port      = var.container_target_port
    transport        = "auto"

    traffic_weight {
      latest_revision = true
      percentage      = 100
    }
  }

  template {
    min_replicas = 0 # scale-to-zero: no traffic, no cost
    max_replicas = 1 # single-instance app; raise once real traffic shows up

    container {
      name   = var.project
      image  = var.container_image
      cpu    = var.container_cpu
      memory = var.container_memory

      env {
        name  = "PORT"
        value = tostring(var.container_target_port)
      }
    }
  }
}
