{#- money(expr): `expr` as an amount with two decimals, in each database's own type. -#}
{% macro money(expr) -%}
{{ dispatch('money', 'acme_sql')(expr) }}
{%- endmacro %}

{% macro default__money(expr) -%}
cast({{ expr }} as decimal(12,2))
{%- endmacro %}

{% macro clickhouse__money(expr) -%}
toDecimal64({{ expr }}, 2)
{%- endmacro %}
