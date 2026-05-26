-- Loyalty Rewards Database Schema
-- 13 tables in FK-dependency order
-- Each table: id UUID PRIMARY KEY, created_at TIMESTAMPTZ DEFAULT now()

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- 1. loyalty_tiers
CREATE TABLE loyalty_tiers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(50) NOT NULL UNIQUE,
    min_points INTEGER NOT NULL DEFAULT 0,
    multiplier NUMERIC(4,2) NOT NULL DEFAULT 1.00,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 2. vendors
CREATE TABLE vendors (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(100) NOT NULL,
    contact_email VARCHAR(255),
    api_key VARCHAR(64),
    active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 3. product_categories
CREATE TABLE product_categories (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(100) NOT NULL,
    slug VARCHAR(100) NOT NULL UNIQUE,
    points_multiplier NUMERIC(4,2) NOT NULL DEFAULT 1.00,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 4. products
CREATE TABLE products (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    vendor_id UUID NOT NULL REFERENCES vendors(id),
    category_id UUID NOT NULL REFERENCES product_categories(id),
    name VARCHAR(200) NOT NULL,
    sku VARCHAR(50) NOT NULL UNIQUE,
    price_cents INTEGER NOT NULL,
    points_value INTEGER NOT NULL DEFAULT 0,
    active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 5. stores
CREATE TABLE stores (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    vendor_id UUID NOT NULL REFERENCES vendors(id),
    name VARCHAR(100) NOT NULL,
    location VARCHAR(200),
    timezone VARCHAR(50) NOT NULL DEFAULT 'UTC',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 6. customers
CREATE TABLE customers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tier_id UUID NOT NULL REFERENCES loyalty_tiers(id),
    email VARCHAR(255) NOT NULL UNIQUE,
    first_name VARCHAR(100) NOT NULL,
    last_name VARCHAR(100) NOT NULL,
    phone VARCHAR(20),
    current_points INTEGER NOT NULL DEFAULT 0,
    lifetime_points INTEGER NOT NULL DEFAULT 0,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 7. purchases
CREATE TABLE purchases (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id UUID NOT NULL REFERENCES customers(id),
    store_id UUID NOT NULL REFERENCES stores(id),
    total_cents INTEGER NOT NULL,
    points_earned INTEGER NOT NULL DEFAULT 0,
    purchased_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 8. purchase_items
CREATE TABLE purchase_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    purchase_id UUID NOT NULL REFERENCES purchases(id),
    product_id UUID NOT NULL REFERENCES products(id),
    quantity INTEGER NOT NULL DEFAULT 1,
    unit_price_cents INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 9. points_transactions
CREATE TABLE points_transactions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id UUID NOT NULL REFERENCES customers(id),
    purchase_id UUID REFERENCES purchases(id),
    points INTEGER NOT NULL,
    transaction_type VARCHAR(20) NOT NULL CHECK (transaction_type IN ('earn', 'redeem', 'adjust', 'expire')),
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 10. rewards
CREATE TABLE rewards (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    description TEXT,
    points_cost INTEGER NOT NULL,
    active BOOLEAN NOT NULL DEFAULT true,
    tier_required UUID REFERENCES loyalty_tiers(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 11. redemptions
CREATE TABLE redemptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    customer_id UUID NOT NULL REFERENCES customers(id),
    reward_id UUID NOT NULL REFERENCES rewards(id),
    points_spent INTEGER NOT NULL,
    redeemed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 12. reward_rules
CREATE TABLE reward_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(100) NOT NULL,
    rule_type VARCHAR(30) NOT NULL CHECK (rule_type IN ('bonus', 'multiplier', 'threshold')),
    condition_json JSONB NOT NULL DEFAULT '{}',
    points_bonus INTEGER NOT NULL DEFAULT 0,
    multiplier NUMERIC(4,2) NOT NULL DEFAULT 1.00,
    active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- 13. referrals
CREATE TABLE referrals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    referrer_id UUID NOT NULL REFERENCES customers(id),
    referee_id UUID NOT NULL REFERENCES customers(id),
    bonus_points INTEGER NOT NULL DEFAULT 0,
    status VARCHAR(20) NOT NULL CHECK (status IN ('pending', 'completed', 'expired')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT no_self_referral CHECK (referrer_id != referee_id)
);
