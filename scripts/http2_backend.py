#!/usr/bin/env python3
"""
Multi-source load tester for canary routing validation.
Simulates requests from different IPs to test Maglev distribution.
"""

import json
import subprocess
import sys
from collections import Counter

def test_from_different_pods(num_requests=100):
    """
    Test routing by making requests from different pods in the cluster.
    Each pod has a different source IP, so Maglev will distribute across backends.
    """
    print(f"Testing canary distribution with {num_requests} requests from different pods...")

    versions = []

    # Get all demo pods (these will be our test clients)
    result = subprocess.run(
        ["kubectl", "get", "pods", "-n", "demo", "-o", "jsonpath={.items[*].metadata.name}"],
        capture_output=True,
        text=True,
        check=True
    )
    pod_names = result.stdout.strip().split()

    if not pod_names:
        print("ERROR: No pods found in demo namespace")
        sys.exit(1)

    print(f"Using {len(pod_names)} pods as test clients: {pod_names}")

    # Make requests from each pod in round-robin fashion
    for i in range(num_requests):
        pod_name = pod_names[i % len(pod_names)]

        try:
            result = subprocess.run(
                ["kubectl", "exec", "-n", "demo", pod_name, "--",
                 "curl", "-s", "http://rauta.rauta-system.svc.cluster.local/api/test"],
                capture_output=True,
                text=True,
                check=False,
                timeout=10  # Prevent hanging
            )
        except subprocess.TimeoutExpired:
            print(f"  Request {i+1} timed out (pod: {pod_name})")
            continue
        except subprocess.SubprocessError as e:
            print(f"  Request {i+1} failed with error: {e} (pod: {pod_name})")
            continue

        if result.returncode == 0:
            output = result.stdout
            # Use proper JSON parsing
            try:
                data = json.loads(output)
                version = data.get("version")
                if version:
                    versions.append(version)
            except json.JSONDecodeError:
                pass  # Silently skip malformed responses

        # Progress indicator
        if (i + 1) % 10 == 0:
            print(f"  Progress: {i + 1}/{num_requests} requests completed", end='\r')

    print(f"\n\nResults after {num_requests} requests:")
    print("=" * 50)

    counter = Counter(versions)
    total = sum(counter.values())

    if total == 0:
        print("ERROR: No successful responses received")
        return

    for version, count in sorted(counter.items()):
        percentage = (count / total) * 100
        print(f"  {version}: {count:3d} requests ({percentage:5.1f}%)")

    # Check if distribution is close to 90/10
    v1_count = counter.get("v1-stable", 0)
    v2_count = counter.get("v2-canary", 0)

    v1_pct = (v1_count / total) * 100
    v2_pct = (v2_count / total) * 100

    print("\n" + "=" * 50)
    print(f"Expected: ~90% v1-stable, ~10% v2-canary")
    print(f"Actual:   {v1_pct:.1f}% v1-stable, {v2_pct:.1f}% v2-canary")

    # Tolerance: ±15% from expected
    if abs(v1_pct - 90) < 15 and abs(v2_pct - 10) < 15:
        print("\n✅ PASS: Distribution matches expected 90/10 split!")
    else:
        print("\n❌ FAIL: Distribution differs significantly from 90/10 split")

if __name__ == "__main__":
    try:
        num_requests = int(sys.argv[1]) if len(sys.argv) > 1 else 100
        if num_requests <= 0:
            print("ERROR: Number of requests must be positive")
            sys.exit(1)
    except ValueError:
        print("ERROR: Invalid number of requests")
        sys.exit(1)

    test_from_different_pods(num_requests)
