# Harbour Outfitters reporting context

Amounts are AUD and exclude GST. Orders have one row per order. Customers have one row per
customer. This recurring report joins the pipeline models by customer_id and groups by region.
The join is evidence of this report's reviewed logic, not proof of every new query.

Upstream table and column comments normally come from dbt or another data pipeline. This demo
seeds them to reproduce that setup. DRE leverages those comments plus recurring report logic;
it does not compete with or replace the pipeline.
