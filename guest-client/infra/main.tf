terraform {
  required_version = ">= 1.5"
  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 7.0"
    }
  }
}

variable "project_id" {
  type        = string
  description = "Google Cloud project for the guest client pilot."
}

variable "channel" {
  type        = string
  description = "Unique short name for the conversation and its dedicated service accounts."

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{3,16}[a-z0-9]$", var.channel))
    error_message = "The channel must be 5–18 lowercase letters, digits, or hyphens, starting with a letter and ending with a letter or digit."
  }
}

locals {
  peers = {
    host   = "client"
    client = "host"
  }
}

resource "google_project_service" "pubsub" {
  project            = var.project_id
  service            = "pubsub.googleapis.com"
  disable_on_destroy = false
}

resource "google_service_account" "peer" {
  for_each     = local.peers
  project      = var.project_id
  account_id   = "${var.channel}-${each.key}"
  display_name = "Boxen guest channel ${each.key}"
}

resource "google_pubsub_topic" "incoming" {
  for_each   = local.peers
  project    = var.project_id
  name       = "${var.channel}-to-${each.key}"
  depends_on = [google_project_service.pubsub]
}

resource "google_pubsub_subscription" "incoming" {
  for_each                   = local.peers
  project                    = var.project_id
  name                       = "${var.channel}-${each.key}-inbox"
  topic                      = google_pubsub_topic.incoming[each.key].id
  message_retention_duration = "2678400s"
  retain_acked_messages      = false

  expiration_policy {
    ttl = ""
  }
}

resource "google_pubsub_topic_iam_member" "publish" {
  for_each = local.peers
  project  = var.project_id
  topic    = google_pubsub_topic.incoming[each.value].name
  role     = "roles/pubsub.publisher"
  member   = "serviceAccount:${google_service_account.peer[each.key].email}"
}

resource "google_pubsub_subscription_iam_member" "consume" {
  for_each     = local.peers
  project      = var.project_id
  subscription = google_pubsub_subscription.incoming[each.key].name
  role         = "roles/pubsub.subscriber"
  member       = "serviceAccount:${google_service_account.peer[each.key].email}"
}

output "endpoints" {
  value = {
    for peer, other in local.peers : peer => {
      account      = google_service_account.peer[peer].email
      publish_to   = google_pubsub_topic.incoming[other].id
      consume_from = google_pubsub_subscription.incoming[peer].id
    }
  }
}
