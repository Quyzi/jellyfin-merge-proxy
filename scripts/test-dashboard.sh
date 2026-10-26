#!/bin/bash
# Test script to validate Grafana dashboard with sample metrics

set -e

echo "=== Jellyfin Merge Proxy - Dashboard Testing ==="
echo

# Check if docker-compose is available
if ! command -v docker-compose &> /dev/null && ! command -v docker &> /dev/null; then
    echo "Error: docker-compose or docker command not found"
    exit 1
fi

COMPOSE_CMD="docker-compose"
if ! command -v docker-compose &> /dev/null; then
    COMPOSE_CMD="docker compose"
fi

cd "$(dirname "$0")/.."

echo "Step 1: Starting observability stack (Prometheus + Grafana)..."
$COMPOSE_CMD up -d prometheus grafana

echo "Waiting for Prometheus to start..."
for i in {1..30}; do
    if curl -s http://localhost:9090/-/ready > /dev/null 2>&1; then
        echo "✓ Prometheus is ready"
        break
    fi
    if [ $i -eq 30 ]; then
        echo "✗ Prometheus failed to start"
        $COMPOSE_CMD logs prometheus
        exit 1
    fi
    sleep 1
done

echo "Waiting for Grafana to start..."
for i in {1..30}; do
    if curl -s http://localhost:3000/api/health > /dev/null 2>&1; then
        echo "✓ Grafana is ready"
        break
    fi
    if [ $i -eq 30 ]; then
        echo "✗ Grafana failed to start"
        $COMPOSE_CMD logs grafana
        exit 1
    fi
    sleep 1
done

echo
echo "Step 2: Checking Grafana dashboard provisioning..."
sleep 2  # Give provisioning a moment

# Check if datasource is provisioned
if curl -s -u admin:admin http://localhost:3000/api/datasources | grep -q "Prometheus"; then
    echo "✓ Prometheus datasource provisioned"
else
    echo "✗ Prometheus datasource not found"
    exit 1
fi

# Check if dashboard is provisioned
if curl -s -u admin:admin http://localhost:3000/api/search | grep -q "Jellyfin Merge Proxy"; then
    echo "✓ Dashboard provisioned successfully"
else
    echo "✗ Dashboard not found"
    exit 1
fi

echo
echo "Step 3: Checking Prometheus configuration..."
if curl -s http://localhost:9090/api/v1/status/config | grep -q "jellyfin-merge-proxy"; then
    echo "✓ Prometheus scrape config loaded"
else
    echo "⚠  Warning: Proxy target not found in Prometheus config"
fi

# Check if alert rules are loaded
if curl -s http://localhost:9090/api/v1/rules | grep -q "jellyfin-merge-proxy"; then
    echo "✓ Alert rules loaded"
else
    echo "⚠  Warning: Alert rules not found"
fi

echo
echo "=== Dashboard Test Complete ==="
echo
echo "Access the services:"
echo "  - Grafana:    http://localhost:3000 (admin/admin)"
echo "  - Prometheus: http://localhost:9090"
echo
echo "To test with the proxy running:"
echo "  1. Start the proxy: docker-compose up -d jellyfin-merge-proxy"
echo "  2. Wait ~30s for first scrape"
echo "  3. Open Grafana dashboard and verify metrics appear"
echo
echo "To stop services: docker-compose down"
echo "To clean up volumes: docker-compose down -v"
