use arcis_imports::*;

#[encrypted]
mod circuits {
    use arcis_imports::*;

    pub struct PayrollInput {
        salary: u64,
        tax_rate: u16, // basis points (10000 = 100%)
        deductions: u64,
    }

    #[instruction]
    pub fn calculate_net_pay(input_ctxt: Enc<Shared, PayrollInput>) -> Enc<Shared, u64> {
        let input = input_ctxt.to_arcis();
        let tax_amount = (input.salary as u128 * input.tax_rate as u128) / 10000;
        let net_pay = input.salary - tax_amount as u64 - input.deductions;
        input_ctxt.owner.from_arcis(net_pay)
    }
}
