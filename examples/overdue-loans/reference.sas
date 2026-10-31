/* UNEXECUTED reference source. No SAS output parity has been established.
   Run from the example root. Base SAS with PROC SQL and ODS EXCEL is assumed.
   Reads only the small CSV fixture; the larger variant is generated for DRE.
   Inspect REPORT-SPEC.md before adapting this to any other input. */
%let report_date='31JAN2026'd;
%let input_dir=inputs;
%let workbook=reference-unexecuted.xlsx;

data customers;
  infile "&input_dir/customers.csv" dsd dlm=',' firstobs=2 truncover;
  length customer_id $80 customer_name $80;
  input customer_id :$80. customer_name :$80.;
run;
data branches;
  infile "&input_dir/branches.csv" dsd dlm=',' firstobs=2 truncover;
  length branch_id $80 branch_name $80;
  input branch_id :$80. version branch_name :$80.;
run;
data loans;
  infile "&input_dir/loans.csv" dsd dlm=',' firstobs=2 truncover;
  length loan_id customer_id branch_id product $80;
  informat opened closed due yymmdd10.;
  format opened closed due yymmdd10.;
  input loan_id :$80. customer_id :$80. branch_id :$80. product :$80.
        opened :yymmdd10. closed :yymmdd10. due :yymmdd10. principal;
run;
data repayments;
  infile "&input_dir/repayments.csv" dsd dlm=',' firstobs=2 truncover;
  length repayment_id loan_id $80;
  informat paid_on yymmdd10.;
  input repayment_id :$80. version loan_id :$80. paid_on :yymmdd10. amount;
run;

/* Resolve duplicate versions BEFORE any join or report-date filter. */
proc sort data=branches; by branch_id descending version; run;
data branches_latest;
  set branches; by branch_id descending version;
  if first.branch_id;
run;
proc sort data=repayments; by repayment_id descending version; run;
data repayments_latest;
  set repayments; by repayment_id descending version;
  if first.repayment_id;
run;
proc sql;
  create table paid as
  select loan_id, sum(amount) as paid
  from repayments_latest where paid_on <= &report_date group by loan_id;
  create table eligible as
  select l.*, c.customer_name, b.branch_name,
         c.customer_id as matched_customer, b.branch_id as matched_branch,
         coalesce(p.paid,0) as paid
  from loans l
  left join customers c on l.customer_id=c.customer_id
  left join branches_latest b on l.branch_id=b.branch_id
  left join paid p on l.loan_id=p.loan_id
  where l.opened <= &report_date
    and (missing(l.closed) or l.closed > &report_date);
quit;
data detail exceptions;
  set eligible;
  length exception $24 bucket $12 due_date $10 branch $80;
  if missing(principal) then exception='MISSING_PRINCIPAL';
  else if principal < 0 then exception='NEGATIVE_PRINCIPAL';
  else if missing(due) then exception='MISSING_DUE';
  else if missing(matched_customer) then exception='MISSING_CUSTOMER';
  else if missing(matched_branch) then exception='MISSING_BRANCH';
  else if principal-paid < 0 then exception='OVERPAID';
  if not missing(exception) then output exceptions;
  else do;
    balance=round(principal-paid,0.01);
    days_overdue=max(0,&report_date-due);
    if days_overdue=0 then bucket='Current';
    else if days_overdue<=30 then bucket='1-30';
    else if days_overdue<=60 then bucket='31-60';
    else if days_overdue<=90 then bucket='61-90';
    else bucket='Over 90';
    due_date=put(due,yymmdd10.);
    branch=branch_name;
    if balance>0 then output detail;
  end;
  format balance comma18.2;
run;
proc sort data=detail; by loan_id; run;
proc sort data=exceptions; by loan_id; run;
proc sql;
  create table summary as
  select catx(' / ',branch,product,bucket) as branch_product_bucket length=120,
         count(*) as accounts, sum(balance) as balance format=comma18.2
  from detail group by branch,product,bucket order by branch,product,bucket;
quit;
ods excel file="&workbook" options(sheet_name='Summary' sheet_interval='none');
proc print data=summary noobs; run;
ods excel options(sheet_name='Detail' sheet_interval='now');
proc print data=detail noobs;
  var loan_id customer_id customer_name branch product due_date days_overdue bucket balance;
run;
ods excel options(sheet_name='Exceptions' sheet_interval='now');
proc print data=exceptions noobs; var loan_id exception; run;
ods excel close;
