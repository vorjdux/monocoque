#!/usr/bin/env python3
"""
Aggregate and analyze Criterion benchmark results.
Generates a comprehensive summary with performance comparisons.
"""
import json
import os
from pathlib import Path
from typing import Dict, List, Tuple
from collections import defaultdict

def parse_criterion_results(criterion_dir: Path) -> Dict:
    """Parse all Criterion benchmark results."""
    results = defaultdict(dict)
    
    for bench_dir in criterion_dir.iterdir():
        if not bench_dir.is_dir() or bench_dir.name == 'report':
            continue
            
        # Try to find estimates.json
        estimates_file = bench_dir / 'base' / 'estimates.json'
        if not estimates_file.exists():
            # Try new.json for latest run
            estimates_file = bench_dir / 'new' / 'estimates.json'
        
        if estimates_file.exists():
            with open(estimates_file) as f:
                data = json.load(f)
                
            bench_name = bench_dir.name
            mean = data.get('mean', {})
            throughput = data.get('throughput', {})
            
            results[bench_name] = {
                'mean': mean.get('point_estimate', 0) / 1e6,  # Convert to ms
                'mean_lower': mean.get('confidence_interval', {}).get('lower_bound', 0) / 1e6,
                'mean_upper': mean.get('confidence_interval', {}).get('upper_bound', 0) / 1e6,
                'throughput': throughput
            }
    
    return results

def format_throughput(value: float, unit: str) -> str:
    """Format throughput with appropriate units."""
    if unit == 'bytes':
        if value >= 1e9:
            return f"{value / 1e9:.2f} GiB/s"
        elif value >= 1e6:
            return f"{value / 1e6:.2f} MiB/s"
        else:
            return f"{value / 1e3:.2f} KiB/s"
    else:  # elements
        if value >= 1e6:
            return f"{value / 1e6:.2f}M msg/s"
        elif value >= 1e3:
            return f"{value / 1e3:.2f}K msg/s"
        else:
            return f"{value:.2f} msg/s"

def generate_summary(results: Dict) -> str:
    """Generate a markdown summary of all benchmarks."""
    output = []
    output.append("# Monocoque Benchmark Results Summary\n")
    output.append(f"Generated: {Path.cwd()}\n")
    
    # Group by category
    categories = defaultdict(list)
    for name, data in sorted(results.items()):
        category = name.split('_')[0]
        categories[category].append((name, data))
    
    for category, benches in sorted(categories.items()):
        output.append(f"\n## {category.upper()}\n")
        
        for name, data in benches:
            mean = data['mean']
            mean_lower = data['mean_lower']
            mean_upper = data['mean_upper']
            
            output.append(f"### {name}")
            output.append(f"- **Mean Time**: {mean:.2f} ms [{mean_lower:.2f}, {mean_upper:.2f}]")
            
            if data['throughput']:
                tp = data['throughput']
                if 'per_iteration' in tp:
                    value = tp['per_iteration']
                    unit = tp.get('unit', 'bytes')
                    # Calculate throughput from mean time
                    throughput = (value / (mean / 1000))  # per second
                    output.append(f"- **Throughput**: {format_throughput(throughput, unit)}")
            output.append("")
    
    # Performance highlights
    output.append("\n## 🚀 Performance Highlights\n")
    
    # Find pipelined benchmarks
    pipelined = [(name, data) for name, data in results.items() if 'pipelined' in name]
    if pipelined:
        output.append("### Pipelined Throughput (with Batching API)")
        for name, data in sorted(pipelined, key=lambda x: x[1]['mean']):
            mean = data['mean']
            if data['throughput'] and 'per_iteration' in data['throughput']:
                tp = data['throughput']
                throughput = (tp['per_iteration'] / (mean / 1000))
                output.append(f"- **{name}**: {format_throughput(throughput, tp.get('unit', 'bytes'))}")
        output.append("")
    
    # Compare monocoque vs rust-zmq
    comparisons = []
    for name, data in results.items():
        if 'monocoque' in name:
            base = name.replace('monocoque', 'zmq_rs').replace('_monocoque_', '_zmq_rs_')
            if base in results:
                mono_time = data['mean']
                zmq_time = results[base]['mean']
                speedup = zmq_time / mono_time
                comparisons.append((name, speedup))
    
    if comparisons:
        output.append("### Monocoque vs rust-zmq Comparison")
        for name, speedup in sorted(comparisons, key=lambda x: x[1], reverse=True):
            if speedup > 1:
                output.append(f"- **{name}**: {speedup:.2f}x faster")
            else:
                output.append(f"- **{name}**: {1/speedup:.2f}x slower")
        output.append("")
    
    return '\n'.join(output)

def main():
    criterion_dir = Path('target/criterion')
    if not criterion_dir.exists():
        print(f"Error: {criterion_dir} not found")
        return
    
    print("Parsing Criterion results...")
    results = parse_criterion_results(criterion_dir)
    
    print(f"Found {len(results)} benchmark results")
    
    summary = generate_summary(results)
    
    # Save to file
    output_file = Path('target/criterion/BENCHMARK_SUMMARY.md')
    output_file.write_text(summary)
    print(f"\nSummary written to: {output_file}")
    
    # Also print to stdout
    print("\n" + summary)

if __name__ == '__main__':
    main()
