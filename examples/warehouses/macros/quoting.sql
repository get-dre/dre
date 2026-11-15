{#- ClickHouse quotes identifiers with backticks: replaces dre_utils' default for it. -#}
{% macro clickhouse__quote_identifier(name) -%}
`{{ name | string | replace('`', '``') }}`
{%- endmacro %}
