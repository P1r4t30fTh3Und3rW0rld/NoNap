# NoNap

NoNap is a Rust microservice that keeps your services awake by periodically pinging configured URLs at randomized intervals. It includes a REST API for remote control and monitoring.

---

## Features

- Concurrently ping multiple URLs with random delay intervals
- REST API to start/stop pinging, manage targets, view status and logs
- Dockerized for easy deployment
- Suitable for deployment on Render or any Docker-compatible host

---

## Configuration

Create a `targets.json` file in the project root with an array of targets.
