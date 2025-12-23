#!/bin/bash
# Example usage script for sai3-analyze

set -e

# Colors for output
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${BLUE}=== sai3-analyze Usage Examples ===${NC}\n"

# Example 1: Analyze all results in current directory
echo -e "${GREEN}Example 1: Analyze all sai3-* directories in current directory${NC}"
echo "$ sai3-analyze --pattern \"sai3-*\" --output results.xlsx"
echo ""

# Example 2: Analyze from specific base directory
echo -e "${GREEN}Example 2: Analyze results from a specific directory${NC}"
echo "$ sai3-analyze \\"
echo "    --pattern \"sai3-*\" \\"
echo "    --base-dir ~/results/GCP-sai3_22-Dec-2025 \\"
echo "    --output gcp-results.xlsx"
echo ""

# Example 3: Analyze specific directories
echo -e "${GREEN}Example 3: Analyze specific directories (comma-separated)${NC}"
echo "$ sai3-analyze \\"
echo "    --dirs sai3-20251222-1744-sai3-resnet50_1hosts,sai3-20251222-1914-sai3-resnet50_8hosts \\"
echo "    --output comparison.xlsx"
echo ""

# Example 4: Using relative paths
echo -e "${GREEN}Example 4: Using relative paths from base directory${NC}"
echo "$ sai3-analyze \\"
echo "    --base-dir ~/results \\"
echo "    --dirs experiment1,experiment2,experiment3 \\"
echo "    --output experiments.xlsx"
echo ""

# Example 5: Quick analysis of recent results
echo -e "${GREEN}Example 5: Quick analysis with default output name${NC}"
echo "$ cd ~/results/GCP-sai3_22-Dec-2025"
echo "$ sai3-analyze --pattern \"sai3-*\""
echo "# Creates sai3bench-results.xlsx in current directory"
echo ""

echo -e "${BLUE}=== Tab Naming Examples ===${NC}\n"
echo "Directory Name                               → Results Tab            → Prepare Tab"
echo "sai3-20251222-1744-sai3-resnet50_1hosts     → 1222-1744-resnet50_1h-R → 1222-1744-resnet50_1h-P"
echo "sai3-20251222-1914-sai3-resnet50_8hosts     → 1222-1914-resnet50_8h-R → 1222-1914-resnet50_8h-P"
echo "sai3-20251222-2129-sai3-unet3d_8hosts       → 1222-2129-unet3d_8h-R   → 1222-2129-unet3d_8h-P"
echo ""

echo -e "${BLUE}=== For More Information ===${NC}"
echo "Run: sai3-analyze --help"
echo "Docs: docs/ANALYZE_TOOL.md"
echo ""
