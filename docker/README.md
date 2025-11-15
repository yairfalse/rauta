# RAUTA Docker Environment

Quick setup for building and testing RAUTA on any platform (macOS, Windows, Linux).

## Quick Start

```bash
# Build RAUTA Gateway (pure Rust userspace proxy)
./docker/build.sh

# Run integration tests
./docker/test.sh

# Run performance benchmarks
./docker/benchmark.sh
```

## Architecture

```
┌─────────────────────────────────────────────┐
│  Docker Host (macOS/Linux/Windows)         │
│                                             │
│  ┌───────────────────────────────────────┐ │
│  │  rauta-net (10.0.1.0/24)              │ │
│  │                                       │ │
│  │  ┌──────────────┐  ┌──────────────┐  │ │
│  │  │  Backend     │  │  RAUTA       │  │ │
│  │  │  10.0.1.10   │  │  10.0.1.5    │  │ │
│  │  │  :8080       │◄─┤  (Gateway)   │  │ │
│  │  └──────────────┘  └──────────────┘  │ │
│  │                     ▲                 │ │
│  │                     │                 │ │
│  │                     │                 │ │
│  │  ┌──────────────────┴──────────────┐ │ │
│  │  │  Client                         │ │ │
│  │  │  10.0.1.20                      │ │ │
│  │  │  (wrk load generator)           │ │ │
│  │  └─────────────────────────────────┘ │ │
│  └───────────────────────────────────────┘ │
└─────────────────────────────────────────────┘
```

## Services

### Backend (rauta-backend)
- **Image**: python:3.11-slim
- **IP**: 10.0.1.10
- **Port**: 8080
- **Purpose**: HTTP server target for routing

### RAUTA (rauta-control)
- **Image**: rauta:latest (custom build)
- **IP**: 10.0.1.5
- **Purpose**: Gateway API controller (pure Rust userspace proxy)

### Client (rauta-client)
- **Image**: williamyeh/wrk
- **IP**: 10.0.1.20
- **Purpose**: Load testing

## Manual Usage

### Build Only
```bash
docker build -t rauta:latest .
```

### Start Services
```bash
docker-compose up -d
```

### View Logs
```bash
# RAUTA control plane
docker-compose logs -f rauta

# Backend
docker-compose logs -f backend

# All services
docker-compose logs -f
```

### Run wrk Load Test
```bash
# Basic test
docker-compose exec client wrk -t4 -c100 -d10s http://10.0.1.10:8080/api/users

# With latency histogram
docker-compose exec client wrk -t12 -c400 -d30s --latency http://10.0.1.10:8080/api/users
```

### Interactive Shell
```bash
# RAUTA container
docker-compose exec rauta /bin/bash

# Check proxy metrics
docker-compose exec rauta curl http://localhost:9090/metrics

# Client container
docker-compose exec client /bin/sh
```

### Stop Services
```bash
docker-compose down
```

### Clean Up
```bash
# Stop and remove containers
docker-compose down

# Remove images
docker rmi rauta:latest

# Remove volumes
docker-compose down -v
```

## Build Details

The Docker build is multi-stage:

### Stage 1: Builder (rust:1.83-bookworm)
- Installs Rust dependencies (pkg-config, libssl-dev)
- Compiles control plane: `control/src/main.rs` → `control`
- Uses cargo dependency caching for faster builds

### Stage 2: Runtime (ubuntu:24.04)
- Minimal image with runtime dependencies (ca-certificates, curl)
- Copies built control plane binary from builder
- Runs as non-root user (rauta)
- Total size: ~100MB (vs ~2GB for builder)

## Troubleshooting

### Build Fails
```bash
# Check Docker version (requires 20.10+)
docker --version

# Check disk space
docker system df

# Clean up
docker system prune -a
```

### Cannot Connect to Backend
```bash
# Check backend is running
docker-compose ps backend

# Check backend health
docker-compose exec backend curl http://localhost:8080

# Check network
docker network inspect rauta_rauta-net
```

### Permission Denied
```bash
# Ensure Docker daemon is running
docker ps

# On Linux, may need sudo
sudo docker-compose up
```

## Performance Expectations

### Docker (SKB Mode)
- Throughput: 50-100k req/s
- Latency p99: 1-5ms
- **Note**: Slower than native XDP due to SKB mode and container overhead

### Native Linux (Driver Mode)
- Throughput: 1M+ req/s
- Latency p99: <100μs
- **Note**: Requires bare metal Linux with XDP-capable NIC

## Next Steps

After validating in Docker:
1. Deploy to bare metal Linux for performance testing
2. Use native XDP mode (not SKB)
3. Benchmark with bpftrace for <10μs latency
4. Profile with perf for optimization

## Files

```
docker/
├── README.md          # This file
├── build.sh               # Build RAUTA image
├── build-quick.sh         # Quick build test (fast iteration)
├── test.sh                # Run integration tests
├── benchmark.sh           # Performance benchmarks
├── Dockerfile.dev         # Development environment
├── Dockerfile.prod        # Production multi-stage build
├── docker-compose.yml     # Development compose (dev container)
└── docker-compose.prod.yml # Production compose (full stack)
```

## References

- [Docker XDP limitations](https://github.com/cilium/cilium/issues/504)
- [XDP SKB vs Native mode](https://lwn.net/Articles/825998/)
- [wrk HTTP benchmarking tool](https://github.com/wg/wrk)
