{% test nonnegative(relation, column) %}
select * from {{ relation }} where {{ column }} < 0
{% endtest %}

{% test reconcile_revenue(relation, expected, tolerance=0.01) %}
select 1 as mismatch
where abs(coalesce((select sum(revenue) from {{ relation }}), 0)
        - coalesce((select sum(revenue) from {{ expected }}), 0)) > {{ tolerance }}
{% endtest %}
