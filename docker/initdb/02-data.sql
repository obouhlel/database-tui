-- Deterministic demo data (setseed) — ~1k customers, 200 products, 100k orders.
SELECT setseed(0.42);

INSERT INTO shop.customers (email, full_name, country, birth_date, created_at, is_active, balance,
                            preferences, tags, last_ip, avatar, notes)
SELECT
    format('user%s@example.com', g),
    (ARRAY['Zoé Martin', 'Élodie Durand', 'Jürgen Müller', '山田 太郎', 'Søren Kierkegaard',
           'María José Pérez', 'Олег Иванов', 'Chloé Lefèvre', 'Ana 🚀 Silva', 'John O''Brien'])[1 + g % 10]
        || ' #' || g,
    (ARRAY['FR', 'DE', 'JP', 'DK', 'ES', 'RU', 'BR', 'IE', 'BE', 'CH'])[1 + g % 10],
    date '1950-01-01' + (random() * 20000)::int,
    timestamptz '2020-01-01 00:00+00' + random() * interval '1800 days',
    g % 13 <> 0,
    round((random() * 5000)::numeric, 2),
    CASE WHEN g % 4 = 0 THEN NULL
         ELSE jsonb_build_object('newsletter', g % 2 = 0, 'lang', (ARRAY['fr', 'en', 'de'])[1 + g % 3],
                                 'theme', jsonb_build_object('dark', g % 3 = 0, 'font_size', 10 + g % 8))
    END,
    CASE WHEN g % 5 = 0 THEN NULL WHEN g % 7 = 0 THEN '{}'::text[]
         ELSE (ARRAY['vip', 'b2b', 'newsletter', 'promo'])[1:1 + g % 4] END,
    CASE WHEN g % 6 = 0 THEN NULL ELSE format('192.168.%s.%s', g % 255, (g * 7) % 255)::inet END,
    CASE WHEN g % 10 = 0 THEN decode(md5(g::text) || md5((g + 1)::text), 'hex') END,
    CASE WHEN g % 3 = 0 THEN ''
         WHEN g % 3 = 1 THEN NULL
         ELSE repeat('Note longue sur plusieurs lignes. ', 5) || E'\nDeuxième ligne.' END
FROM generate_series(1, 1000) g;

INSERT INTO shop.products (sku, name, description, price, stock, weight_kg, attributes, released_on)
SELECT
    format('SKU-%s', lpad(g::text, 5, '0')),
    (ARRAY['Clavier', 'Souris', 'Écran', 'Câble', 'Casque', 'Webcam', 'Station', 'Chargeur'])[1 + g % 8]
        || ' modèle ' || g,
    CASE WHEN g % 9 = 0 THEN NULL ELSE 'Description du produit ' || g || '. ' || repeat('Lorem ipsum. ', g % 10) END,
    round((5 + random() * 995)::numeric, 2),
    (random() * 500)::int,
    CASE WHEN g % 11 = 0 THEN NULL ELSE round((random() * 10)::numeric, 3)::real END,
    jsonb_build_object('color', (ARRAY['noir', 'blanc', 'gris', 'rouge'])[1 + g % 4],
                       'warranty_years', 1 + g % 3,
                       'dimensions', jsonb_build_array(10 + g % 50, 5 + g % 20, 1 + g % 10)),
    date '2018-01-01' + (random() * 2500)::int
FROM generate_series(1, 200) g;

INSERT INTO shop.orders (customer_id, status, ordered_at, shipped_at, processing_time, notes)
SELECT
    1 + (random() * 999)::int,
    (ARRAY['pending', 'paid', 'shipped', 'delivered', 'cancelled']::shop.order_status[])[1 + (random() * 4)::int],
    t,
    CASE WHEN random() < 0.6 THEN (t + random() * interval '5 days')::timestamp END,
    CASE WHEN random() < 0.7 THEN make_interval(days => (random() * 3)::int, hours => (random() * 23)::int,
                                                mins => (random() * 59)::int) END,
    CASE WHEN g % 50 = 0 THEN 'Livrer avant 18h' END
FROM generate_series(1, 100000) g,
     LATERAL (SELECT timestamptz '2023-01-01 00:00+00' + random() * interval '1000 days' + g * interval '0 s' AS t) s;

INSERT INTO shop.order_items (order_id, product_id, quantity, unit_price)
SELECT DISTINCT ON (o.id, p.pid) o.id, p.pid, 1 + (random() * 4)::int, pr.price
FROM shop.orders o
CROSS JOIN LATERAL (SELECT 1 + ((o.id * k * 37) % 200)::int AS pid FROM generate_series(1, 1 + (o.id % 4)::int) k) p
JOIN shop.products pr ON pr.id = p.pid;

CALL shop.recompute_totals();
REFRESH MATERIALIZED VIEW shop.mv_daily_sales;

INSERT INTO shop.event_log (happened_at, level, message)
SELECT now() - g * interval '1 minute', (ARRAY['INFO', 'WARN', 'ERROR'])[1 + g % 3], 'Événement ' || g
FROM generate_series(1, 500) g;

INSERT INTO shop.measurements (sensor, taken_on, value)
SELECT 'sensor-' || (g % 5), date '2025-01-01' + (g % 700), random() * 100
FROM generate_series(1, 5000) g;

INSERT INTO shop.wide_metrics
SELECT g, 'ligne ' || g, VARIADIC_FILL.*
FROM generate_series(1, 300) g,
     LATERAL (SELECT
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3), round((random()*1000)::numeric, 3),
        round((random()*1000)::numeric, 3) WHERE g > 0) AS VARIADIC_FILL;

INSERT INTO hr.departments VALUES (1, 'Direction'), (2, 'Tech'), (3, 'Ventes'), (4, 'Support');
INSERT INTO hr.employees VALUES
    (1, 1, NULL, 'Camille', 'Bernard', '2015-03-01', 9500, '09:00', '{"langs": ["fr", "en"]}'),
    (2, 2, 1, 'Hugo', 'Petit', '2017-06-15', 6200, '09:30', '{"langs": ["rust", "sql"], "level": "senior"}'),
    (3, 2, 2, 'Inès', 'Roux', '2020-09-01', 4800, '10:00', '{"langs": ["go"]}'),
    (4, 3, 1, 'Lucas', 'Moreau', '2018-01-10', 5100, '08:30', NULL),
    (5, 4, 4, 'Léa', 'Fournier', '2021-11-22', 3900, NULL, '{}');

INSERT INTO audit.changes (table_name, operation, row_data)
SELECT 'shop.customers', (ARRAY['I', 'U', 'D'])[1 + g % 3], jsonb_build_object('id', g, 'by', 'seed')
FROM generate_series(1, 50) g;

INSERT INTO public.settings VALUES ('app.name', 'pgtui demo'), ('app.version', '1'), ('empty', ''), ('null', NULL);

ANALYZE;
