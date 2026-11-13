#!/usr/bin/env bash
# pre-commit's own run of this repository's `scrutate-dupes` hook (`pre-commit try-repo`): it
# passes on a repository without duplicate functions, and fails, naming both copies, once a
# commit adds one. Needs pre-commit 4.4 or newer and scrutate on PATH.
#   tools/test_precommit.sh /path/to/this/repository
set -euo pipefail
src=$(cd "$1" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
git init -q
git config user.email ci@example.invalid
git config user.name ci
mkdir -p src
cat > src/orders.py << 'EOF'
def order_total(order):
    subtotal = 0
    for item in order.items:
        price = item.unit_price * item.quantity
        if item.discount_rate:
            price = price - price * item.discount_rate
        if item.taxable:
            price = price + price * order.tax_rate
        subtotal = subtotal + price
    if order.coupon and order.coupon.amount < subtotal:
        subtotal = subtotal - order.coupon.amount
    if order.shipping_method == "express":
        subtotal = subtotal + order.express_fee
    elif order.shipping_method == "standard" and subtotal < order.free_shipping_floor:
        subtotal = subtotal + order.standard_fee
    if order.gift_wrap:
        subtotal = subtotal + order.gift_wrap_fee * len(order.items)
    if order.loyalty_points and order.loyalty_points > order.points_floor:
        subtotal = subtotal - order.loyalty_points * order.point_value
    if subtotal < 0:
        subtotal = 0
    return round(subtotal, 2)
EOF
cat > src/report.py << 'EOF'
def summary(rows):
    return {"count": len(rows), "first": rows[0] if rows else None}
EOF
git add -A
git commit -qm "no duplicates"
pre-commit try-repo "$src" scrutate-dupes --all-files > clean.log 2>&1 || {
  cat clean.log
  echo "the hook failed on a repository without duplicate functions"
  exit 1
}
cat clean.log
sed 's/def order_total/def invoice_total/' src/orders.py > src/invoices.py
git add -A
git commit -qm "a copy"
if pre-commit try-repo "$src" scrutate-dupes --all-files > dupes.log 2>&1; then
  cat dupes.log
  echo "the hook passed on a repository with a duplicate function"
  exit 1
fi
cat dupes.log
if ! grep -q "src/orders.py" dupes.log || ! grep -q "src/invoices.py" dupes.log; then
  echo "the failure does not name both copies"
  exit 1
fi
echo "pre-commit: the hook passes without duplicates and fails, naming both, with one"
