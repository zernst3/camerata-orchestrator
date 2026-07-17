output "app_url" {
  value       = "https://${azurerm_container_app.main.ingress[0].fqdn}"
  description = "The public URL of the deployed app."
}

output "resource_group_name" {
  value       = azurerm_resource_group.main.name
  description = "The resource group holding every resource this module created."
}
