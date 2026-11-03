{% test nonnegative(relation, column='amount') %}
select * from {{ relation }} where {{ column }} < 0
{% endtest %}

{% test row_count(relation, expected) %}
select 1 as mismatch where (select count(*) from {{ relation }}) != {{ expected }}
{% endtest %}

{% test reconcile_total(relation, expected, tolerance=0.01) %}
select 1 as mismatch where abs((select sum(amount) from {{ relation }})
                            - (select sum(amount) from {{ expected }})) > {{ tolerance }}
{% endtest %}

{% test execution_count(relation) %}
select * from {{ relation }} where execution != 3
{% endtest %}

{% test broken_column(relation) %}
select missing_runtime_column from {{ relation }}
{% endtest %}
