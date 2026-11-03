-- 爱发电订单兑换台账：一单一次，主键就是兑现的凭据本身。
--
-- 只有服务端的 folio-redeem 函数读写（service_role 绕过 RLS），所以这里不给
-- anon/authenticated 任何 GRANT —— 浏览器不该看得到谁兑过什么。
CREATE TABLE folio_redemptions (
  out_trade_no TEXT        PRIMARY KEY,
  device       TEXT        NOT NULL,
  plan_id      TEXT        NOT NULL,
  buyer_id     TEXT        NOT NULL,
  granted_days INTEGER     NOT NULL,
  paid_amount  NUMERIC(10, 2),
  redeemed_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX folio_redemptions_device_idx ON folio_redemptions (device);
