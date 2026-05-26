// Loyalty DB Seeder — deterministic test data with fixed UUIDs
// for reproducible Merkle trees in ict-rs loyalty_rewards E2E test.

import pg from "pg";
import { readFileSync } from "fs";
import { dirname, join } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));

const { Client } = pg;

// ---------------------------------------------------------------------------
// Fixed UUIDs — deterministic so Merkle trees are reproducible
// ---------------------------------------------------------------------------

const TIERS = {
  bronze: "a0000000-0000-0000-0000-000000000001",
  silver: "a0000000-0000-0000-0000-000000000002",
  gold:   "a0000000-0000-0000-0000-000000000003",
};

const VENDORS = {
  greenLeaf:  "b0000000-0000-0000-0000-000000000001",
  techMart:   "b0000000-0000-0000-0000-000000000002",
  dailyBrew:  "b0000000-0000-0000-0000-000000000003",
};

const CATEGORIES = {
  groceries:    "c0000000-0000-0000-0000-000000000001",
  electronics:  "c0000000-0000-0000-0000-000000000002",
  beverages:    "c0000000-0000-0000-0000-000000000003",
  supplements:  "c0000000-0000-0000-0000-000000000004",
};

const PRODUCTS = {
  organicApples:  "d0000000-0000-0000-0000-000000000001",
  usbCable:       "d0000000-0000-0000-0000-000000000002",
  coldBrew:       "d0000000-0000-0000-0000-000000000003",
  proteinPowder:  "d0000000-0000-0000-0000-000000000004",
  blueberries:    "d0000000-0000-0000-0000-000000000005",
  wirelessMouse:  "d0000000-0000-0000-0000-000000000006",
  greenTea:       "d0000000-0000-0000-0000-000000000007",
  vitaminD:       "d0000000-0000-0000-0000-000000000008",
};

const STORES = {
  greenLeafMain:  "e0000000-0000-0000-0000-000000000001",
  techMartOnline: "e0000000-0000-0000-0000-000000000002",
};

const CUSTOMERS = {
  jane:    "f0000000-0000-0000-0000-000000000001",
  bob:     "f0000000-0000-0000-0000-000000000002",
  alice:   "f0000000-0000-0000-0000-000000000003",
  charlie: "f0000000-0000-0000-0000-000000000004",
  diana:   "f0000000-0000-0000-0000-000000000005",
};

const PUR = {
  p1: "10000000-0000-0000-0000-000000000001",
  p2: "10000000-0000-0000-0000-000000000002",
  p3: "10000000-0000-0000-0000-000000000003",
  p4: "10000000-0000-0000-0000-000000000004",
  p5: "10000000-0000-0000-0000-000000000005",
  p6: "10000000-0000-0000-0000-000000000006",
  p7: "10000000-0000-0000-0000-000000000007",
  p8: "10000000-0000-0000-0000-000000000008",
  p9: "10000000-0000-0000-0000-000000000009",
  p10:"10000000-0000-0000-0000-000000000010",
};

const PI = {
  i1:  "20000000-0000-0000-0000-000000000001",
  i2:  "20000000-0000-0000-0000-000000000002",
  i3:  "20000000-0000-0000-0000-000000000003",
  i4:  "20000000-0000-0000-0000-000000000004",
  i5:  "20000000-0000-0000-0000-000000000005",
  i6:  "20000000-0000-0000-0000-000000000006",
  i7:  "20000000-0000-0000-0000-000000000007",
  i8:  "20000000-0000-0000-0000-000000000008",
  i9:  "20000000-0000-0000-0000-000000000009",
  i10: "20000000-0000-0000-0000-000000000010",
  i11: "20000000-0000-0000-0000-000000000011",
  i12: "20000000-0000-0000-0000-000000000012",
};

const PT = {
  t1:  "30000000-0000-0000-0000-000000000001",
  t2:  "30000000-0000-0000-0000-000000000002",
  t3:  "30000000-0000-0000-0000-000000000003",
  t4:  "30000000-0000-0000-0000-000000000004",
  t5:  "30000000-0000-0000-0000-000000000005",
  t6:  "30000000-0000-0000-0000-000000000006",
  t7:  "30000000-0000-0000-0000-000000000007",
  t8:  "30000000-0000-0000-0000-000000000008",
  t9:  "30000000-0000-0000-0000-000000000009",
  t10: "30000000-0000-0000-0000-000000000010",
  t11: "30000000-0000-0000-0000-000000000011",
  t12: "30000000-0000-0000-0000-000000000012",
  t13: "30000000-0000-0000-0000-000000000013",
  t14: "30000000-0000-0000-0000-000000000014",
  t15: "30000000-0000-0000-0000-000000000015",
  t16: "30000000-0000-0000-0000-000000000016",
  t17: "30000000-0000-0000-0000-000000000017",
  t18: "30000000-0000-0000-0000-000000000018",
  t19: "30000000-0000-0000-0000-000000000019",
  t20: "30000000-0000-0000-0000-000000000020",
};

const REWARDS_IDS = {
  r1: "40000000-0000-0000-0000-000000000001",
  r2: "40000000-0000-0000-0000-000000000002",
  r3: "40000000-0000-0000-0000-000000000003",
  r4: "40000000-0000-0000-0000-000000000004",
};

const REDEMPTIONS_IDS = {
  rd1: "50000000-0000-0000-0000-000000000001",
  rd2: "50000000-0000-0000-0000-000000000002",
  rd3: "50000000-0000-0000-0000-000000000003",
};

const RULES = {
  rl1: "60000000-0000-0000-0000-000000000001",
  rl2: "60000000-0000-0000-0000-000000000002",
};

const REFERRALS_IDS = {
  rf1: "70000000-0000-0000-0000-000000000001",
  rf2: "70000000-0000-0000-0000-000000000002",
};

// ---------------------------------------------------------------------------
// Helper: insert one row at a time to avoid Postgres parameter ambiguity
// ---------------------------------------------------------------------------

async function ins(client, sql, params) {
  await client.query(sql, params);
}

// ---------------------------------------------------------------------------
// Seed function
// ---------------------------------------------------------------------------

async function seed() {
  const url = process.env.DATABASE_URL;
  if (!url) throw new Error("DATABASE_URL not set");

  const client = new Client({ connectionString: url });
  await client.connect();
  console.log("Connected to", url);

  // Run schema
  const schema = readFileSync(join(__dirname, "schema.sql"), "utf8");
  await client.query(schema);
  console.log("Schema applied (13 tables)");

  // Fixed timestamp for reproducibility
  const ts = "2025-01-15T12:00:00Z";

  // --- loyalty_tiers ---
  const tierSql = `INSERT INTO loyalty_tiers (id, name, min_points, multiplier, description, created_at) VALUES ($1, $2, $3, $4, $5, $6)`;
  await ins(client, tierSql, [TIERS.bronze, 'Bronze', 0, 1.00, 'Entry tier', ts]);
  await ins(client, tierSql, [TIERS.silver, 'Silver', 500, 1.50, 'Mid tier with 1.5x multiplier', ts]);
  await ins(client, tierSql, [TIERS.gold, 'Gold', 2000, 2.00, 'Top tier with 2x multiplier', ts]);

  // --- vendors ---
  const vendorSql = `INSERT INTO vendors (id, name, contact_email, api_key, active, created_at) VALUES ($1, $2, $3, $4, $5, $6)`;
  await ins(client, vendorSql, [VENDORS.greenLeaf, 'GreenLeaf Market', 'contact@greenleaf.test', 'glk_test_001', true, ts]);
  await ins(client, vendorSql, [VENDORS.techMart, 'TechMart', 'support@techmart.test', 'tmk_test_002', true, ts]);
  await ins(client, vendorSql, [VENDORS.dailyBrew, 'Daily Brew', 'info@dailybrew.test', 'dbk_test_003', true, ts]);

  // --- product_categories ---
  const catSql = `INSERT INTO product_categories (id, name, slug, points_multiplier, created_at) VALUES ($1, $2, $3, $4, $5)`;
  await ins(client, catSql, [CATEGORIES.groceries, 'Groceries', 'groceries', 1.00, ts]);
  await ins(client, catSql, [CATEGORIES.electronics, 'Electronics', 'electronics', 1.25, ts]);
  await ins(client, catSql, [CATEGORIES.beverages, 'Beverages', 'beverages', 1.00, ts]);
  await ins(client, catSql, [CATEGORIES.supplements, 'Supplements', 'supplements', 1.50, ts]);

  // --- products ---
  const prodSql = `INSERT INTO products (id, vendor_id, category_id, name, sku, price_cents, points_value, active, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)`;
  await ins(client, prodSql, [PRODUCTS.organicApples, VENDORS.greenLeaf, CATEGORIES.groceries, 'Organic Apples (1kg)', 'GL-APL-001', 599, 10, true, ts]);
  await ins(client, prodSql, [PRODUCTS.usbCable, VENDORS.techMart, CATEGORIES.electronics, 'USB-C Cable 2m', 'TM-USB-001', 1299, 25, true, ts]);
  await ins(client, prodSql, [PRODUCTS.coldBrew, VENDORS.dailyBrew, CATEGORIES.beverages, 'Cold Brew Coffee', 'DB-CBR-001', 499, 8, true, ts]);
  await ins(client, prodSql, [PRODUCTS.proteinPowder, VENDORS.greenLeaf, CATEGORIES.supplements, 'Whey Protein 1kg', 'GL-WHP-001', 3999, 75, true, ts]);
  await ins(client, prodSql, [PRODUCTS.blueberries, VENDORS.greenLeaf, CATEGORIES.groceries, 'Organic Blueberries', 'GL-BLU-001', 799, 15, true, ts]);
  await ins(client, prodSql, [PRODUCTS.wirelessMouse, VENDORS.techMart, CATEGORIES.electronics, 'Wireless Mouse', 'TM-WMS-001', 2499, 50, true, ts]);
  await ins(client, prodSql, [PRODUCTS.greenTea, VENDORS.dailyBrew, CATEGORIES.beverages, 'Green Tea Box', 'DB-GTE-001', 699, 12, true, ts]);
  await ins(client, prodSql, [PRODUCTS.vitaminD, VENDORS.greenLeaf, CATEGORIES.supplements, 'Vitamin D 60ct', 'GL-VTD-001', 1499, 30, true, ts]);

  // --- stores ---
  const storeSql = `INSERT INTO stores (id, vendor_id, name, location, timezone, created_at) VALUES ($1, $2, $3, $4, $5, $6)`;
  await ins(client, storeSql, [STORES.greenLeafMain, VENDORS.greenLeaf, 'GreenLeaf Main St', '123 Main Street, Portland OR', 'America/Los_Angeles', ts]);
  await ins(client, storeSql, [STORES.techMartOnline, VENDORS.techMart, 'TechMart Online', 'online', 'UTC', ts]);

  // --- customers ---
  // Jane: Silver tier, 875 points (will claim first via Merkle proof)
  // Bob:  Gold tier, 1200 points (will update to 1525 then claim)
  const custSql = `INSERT INTO customers (id, tier_id, email, first_name, last_name, phone, current_points, lifetime_points, joined_at, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)`;
  await ins(client, custSql, [CUSTOMERS.jane, TIERS.silver, 'jane@example.test', 'Jane', 'Doe', '+15551001', 875, 1250, ts, ts]);
  await ins(client, custSql, [CUSTOMERS.bob, TIERS.gold, 'bob@example.test', 'Bob', 'Smith', '+15551002', 1200, 3400, ts, ts]);
  await ins(client, custSql, [CUSTOMERS.alice, TIERS.silver, 'alice@example.test', 'Alice', 'Johnson', '+15551003', 620, 980, ts, ts]);
  await ins(client, custSql, [CUSTOMERS.charlie, TIERS.bronze, 'charlie@example.test', 'Charlie', 'Brown', '+15551004', 150, 150, ts, ts]);
  await ins(client, custSql, [CUSTOMERS.diana, TIERS.gold, 'diana@example.test', 'Diana', 'Prince', '+15551005', 2100, 5200, ts, ts]);

  // --- purchases ---
  const purSql = `INSERT INTO purchases (id, customer_id, store_id, total_cents, points_earned, purchased_at, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)`;
  await ins(client, purSql, [PUR.p1, CUSTOMERS.jane, STORES.greenLeafMain, 2397, 45, ts, ts]);
  await ins(client, purSql, [PUR.p2, CUSTOMERS.jane, STORES.techMartOnline, 1299, 25, ts, ts]);
  await ins(client, purSql, [PUR.p3, CUSTOMERS.bob, STORES.greenLeafMain, 4498, 85, ts, ts]);
  await ins(client, purSql, [PUR.p4, CUSTOMERS.bob, STORES.techMartOnline, 2499, 50, ts, ts]);
  await ins(client, purSql, [PUR.p5, CUSTOMERS.alice, STORES.greenLeafMain, 1298, 23, ts, ts]);
  await ins(client, purSql, [PUR.p6, CUSTOMERS.alice, STORES.techMartOnline, 2499, 50, ts, ts]);
  await ins(client, purSql, [PUR.p7, CUSTOMERS.charlie, STORES.greenLeafMain, 599, 10, ts, ts]);
  await ins(client, purSql, [PUR.p8, CUSTOMERS.diana, STORES.greenLeafMain, 5498, 105, ts, ts]);
  await ins(client, purSql, [PUR.p9, CUSTOMERS.diana, STORES.techMartOnline, 1299, 25, ts, ts]);
  await ins(client, purSql, [PUR.p10, CUSTOMERS.bob, STORES.greenLeafMain, 699, 12, ts, ts]);

  // --- purchase_items ---
  const piSql = `INSERT INTO purchase_items (id, purchase_id, product_id, quantity, unit_price_cents, created_at) VALUES ($1, $2, $3, $4, $5, $6)`;
  await ins(client, piSql, [PI.i1, PUR.p1, PRODUCTS.organicApples, 2, 599, ts]);
  await ins(client, piSql, [PI.i2, PUR.p1, PRODUCTS.blueberries, 1, 799, ts]);
  await ins(client, piSql, [PI.i3, PUR.p2, PRODUCTS.usbCable, 1, 1299, ts]);
  await ins(client, piSql, [PI.i4, PUR.p3, PRODUCTS.proteinPowder, 1, 3999, ts]);
  await ins(client, piSql, [PI.i5, PUR.p3, PRODUCTS.organicApples, 1, 599, ts]);
  await ins(client, piSql, [PI.i6, PUR.p4, PRODUCTS.wirelessMouse, 1, 2499, ts]);
  await ins(client, piSql, [PI.i7, PUR.p5, PRODUCTS.blueberries, 1, 799, ts]);
  await ins(client, piSql, [PI.i8, PUR.p5, PRODUCTS.coldBrew, 1, 499, ts]);
  await ins(client, piSql, [PI.i9, PUR.p6, PRODUCTS.wirelessMouse, 1, 2499, ts]);
  await ins(client, piSql, [PI.i10, PUR.p7, PRODUCTS.organicApples, 1, 599, ts]);
  await ins(client, piSql, [PI.i11, PUR.p8, PRODUCTS.proteinPowder, 1, 3999, ts]);
  await ins(client, piSql, [PI.i12, PUR.p8, PRODUCTS.vitaminD, 1, 1499, ts]);

  // --- points_transactions ---
  const ptSql = `INSERT INTO points_transactions (id, customer_id, purchase_id, points, transaction_type, description, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)`;
  const ptNopur = `INSERT INTO points_transactions (id, customer_id, purchase_id, points, transaction_type, description, created_at) VALUES ($1, $2, NULL, $3, $4, $5, $6)`;
  await ins(client, ptSql, [PT.t1, CUSTOMERS.jane, PUR.p1, 45, 'earn', 'Purchase at GreenLeaf', ts]);
  await ins(client, ptSql, [PT.t2, CUSTOMERS.jane, PUR.p2, 25, 'earn', 'Purchase at TechMart', ts]);
  await ins(client, ptSql, [PT.t3, CUSTOMERS.bob, PUR.p3, 85, 'earn', 'Purchase at GreenLeaf', ts]);
  await ins(client, ptSql, [PT.t4, CUSTOMERS.bob, PUR.p4, 50, 'earn', 'Purchase at TechMart', ts]);
  await ins(client, ptSql, [PT.t5, CUSTOMERS.alice, PUR.p5, 23, 'earn', 'Purchase at GreenLeaf', ts]);
  await ins(client, ptSql, [PT.t6, CUSTOMERS.alice, PUR.p6, 50, 'earn', 'Purchase at TechMart', ts]);
  await ins(client, ptSql, [PT.t7, CUSTOMERS.charlie, PUR.p7, 10, 'earn', 'Purchase at GreenLeaf', ts]);
  await ins(client, ptSql, [PT.t8, CUSTOMERS.diana, PUR.p8, 105, 'earn', 'Purchase at GreenLeaf', ts]);
  await ins(client, ptSql, [PT.t9, CUSTOMERS.diana, PUR.p9, 25, 'earn', 'Purchase at TechMart', ts]);
  await ins(client, ptSql, [PT.t10, CUSTOMERS.bob, PUR.p10, 12, 'earn', 'Purchase at GreenLeaf', ts]);
  // Non-purchase transactions
  await ins(client, ptNopur, [PT.t11, CUSTOMERS.jane, 500, 'earn', 'Welcome bonus', ts]);
  await ins(client, ptNopur, [PT.t12, CUSTOMERS.jane, 305, 'earn', 'Referral bonus', ts]);
  await ins(client, ptNopur, [PT.t13, CUSTOMERS.bob, 1000, 'earn', 'Welcome bonus', ts]);
  await ins(client, ptNopur, [PT.t14, CUSTOMERS.bob, 53, 'adjust', 'Points correction', ts]);
  await ins(client, ptNopur, [PT.t15, CUSTOMERS.alice, 500, 'earn', 'Welcome bonus', ts]);
  await ins(client, ptNopur, [PT.t16, CUSTOMERS.alice, -3, 'adjust', 'Rounding correction', ts]);
  await ins(client, ptNopur, [PT.t17, CUSTOMERS.charlie, 100, 'earn', 'Welcome bonus', ts]);
  await ins(client, ptNopur, [PT.t18, CUSTOMERS.charlie, 40, 'earn', 'Birthday bonus', ts]);
  await ins(client, ptNopur, [PT.t19, CUSTOMERS.diana, 2000, 'earn', 'Welcome bonus', ts]);
  await ins(client, ptNopur, [PT.t20, CUSTOMERS.diana, -30, 'redeem', 'Small reward', ts]);

  // --- rewards ---
  const rwdSql = `INSERT INTO rewards (id, name, description, points_cost, active, tier_required, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7)`;
  await ins(client, rwdSql, [REWARDS_IDS.r1, 'Free Coffee', 'One free cold brew', 100, true, null, ts]);
  await ins(client, rwdSql, [REWARDS_IDS.r2, '10% Off Electronics', '10% off next e-purchase', 500, true, TIERS.silver, ts]);
  await ins(client, rwdSql, [REWARDS_IDS.r3, 'Free Protein Powder', 'One free 1kg whey', 1500, true, TIERS.gold, ts]);
  await ins(client, rwdSql, [REWARDS_IDS.r4, 'VIP Event Access', 'Exclusive tasting event', 3000, true, TIERS.gold, ts]);

  // --- redemptions ---
  const redSql = `INSERT INTO redemptions (id, customer_id, reward_id, points_spent, redeemed_at, created_at) VALUES ($1, $2, $3, $4, $5, $6)`;
  await ins(client, redSql, [REDEMPTIONS_IDS.rd1, CUSTOMERS.jane, REWARDS_IDS.r1, 100, ts, ts]);
  await ins(client, redSql, [REDEMPTIONS_IDS.rd2, CUSTOMERS.bob, REWARDS_IDS.r2, 500, ts, ts]);
  await ins(client, redSql, [REDEMPTIONS_IDS.rd3, CUSTOMERS.diana, REWARDS_IDS.r1, 100, ts, ts]);

  // --- reward_rules ---
  const rulSql = `INSERT INTO reward_rules (id, name, rule_type, condition_json, points_bonus, multiplier, active, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)`;
  await ins(client, rulSql, [RULES.rl1, 'Weekend Bonus', 'bonus', '{"days":["saturday","sunday"]}', 25, 1.00, true, ts]);
  await ins(client, rulSql, [RULES.rl2, 'Gold 2x Multiplier', 'multiplier', '{"tier":"gold"}', 0, 2.00, true, ts]);

  // --- referrals ---
  const refSql = `INSERT INTO referrals (id, referrer_id, referee_id, bonus_points, status, created_at) VALUES ($1, $2, $3, $4, $5, $6)`;
  await ins(client, refSql, [REFERRALS_IDS.rf1, CUSTOMERS.jane, CUSTOMERS.alice, 250, 'completed', ts]);
  await ins(client, refSql, [REFERRALS_IDS.rf2, CUSTOMERS.bob, CUSTOMERS.charlie, 250, 'pending', ts]);

  // Verify counts
  const tables = [
    'loyalty_tiers', 'vendors', 'product_categories', 'products', 'stores',
    'customers', 'purchases', 'purchase_items', 'points_transactions',
    'rewards', 'redemptions', 'reward_rules', 'referrals'
  ];
  let totalRows = 0;
  for (const t of tables) {
    const res = await client.query(`SELECT count(*) FROM ${t}`);
    const count = parseInt(res.rows[0].count);
    totalRows += count;
    console.log(`  ${t}: ${count} rows`);
  }
  console.log(`Total: ${totalRows} rows across ${tables.length} tables`);

  await client.end();
  console.log("Seed complete");
}

seed().catch(err => {
  console.error("Seed failed:", err);
  process.exit(1);
});
