use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};
use arcium_anchor::prelude::*;

const COMP_DEF_OFFSET_CALCULATE_NET_PAY: u32 = comp_def_offset("calculate_net_pay");

declare_id!("5w4okCHwmXCS84u93nBQWNfZ3gRVV2UTYt4diyUR7d8c");

#[arcium_program]
pub mod confidential_payroll {
    use super::*;

    pub fn init_calculate_net_pay_comp_def(ctx: Context<InitCalculateNetPayCompDef>) -> Result<()> {
        init_comp_def(ctx.accounts, true, 0, None, None)?;
        Ok(())
    }

    pub fn initialize_payroll(
        ctx: Context<InitializePayroll>,
        payroll_id: String,
        tax_rate: u16,
    ) -> Result<()> {
        let payroll = &mut ctx.accounts.payroll;
        payroll.authority = ctx.accounts.authority.key();
        payroll.payroll_id = payroll_id;
        payroll.payment_token = ctx.accounts.payment_token.key();
        payroll.employee_count = 0;
        payroll.is_active = true;
        payroll.tax_rate = tax_rate;
        payroll.total_funds = 0;
        payroll.vault_bump = ctx.bumps.payroll_vault;

        emit!(PayrollInitialized {
            payroll_id: payroll.payroll_id.clone(),
            authority: payroll.authority,
            tax_rate: payroll.tax_rate,
        });

        Ok(())
    }

    pub fn add_employee(
        ctx: Context<AddEmployee>,
        employee_id: String,
        salary_amount: u64,
        deductions: u64,
        payment_frequency: PaymentFrequency,
    ) -> Result<()> {
        let payroll = &mut ctx.accounts.payroll;
        let employee = &mut ctx.accounts.employee;

        require!(payroll.is_active, ErrorCode::PayrollInactive);
        require!(employee_id.len() <= 64, ErrorCode::InvalidEmployeeId);

        employee.payroll = payroll.key();
        employee.employee_id = employee_id.clone();
        employee.wallet = ctx.accounts.employee_wallet.key();
        employee.salary_amount = salary_amount;
        employee.deductions = deductions;
        employee.payment_frequency = payment_frequency;
        // Set last_payment to 0 to allow immediate first payment
        employee.last_payment = 0;
        employee.is_active = true;

        payroll.employee_count += 1;

        emit!(EmployeeAdded {
            payroll_id: payroll.payroll_id.clone(),
            employee_id,
            salary_amount,
            deductions,
        });

        Ok(())
    }

    pub fn calculate_net_pay(
        ctx: Context<CalculateNetPay>,
        computation_offset: u64,
        encrypted_salary: [u8; 32],
        encrypted_tax_rate: [u8; 32],
        encrypted_deductions: [u8; 32],
        pub_key: [u8; 32],
        nonce: u128,
    ) -> Result<()> {
        ctx.accounts.sign_pda_account.bump = ctx.bumps.sign_pda_account;
        let args = vec![
            Argument::ArcisPubkey(pub_key),
            Argument::PlaintextU128(nonce),
            Argument::EncryptedU64(encrypted_salary),
            Argument::EncryptedU16(encrypted_tax_rate),
            Argument::EncryptedU64(encrypted_deductions),
        ];

        queue_computation(
            ctx.accounts,
            computation_offset,
            args,
            None,
            vec![CalculateNetPayCallback::callback_ix(&[])],
        )?;

        Ok(())
    }

    #[arcium_callback(encrypted_ix = "calculate_net_pay")]
    pub fn calculate_net_pay_callback(
        ctx: Context<CalculateNetPayCallback>,
        output: ComputationOutputs<CalculateNetPayOutput>,
    ) -> Result<()> {
        let net_pay = match output {
            ComputationOutputs::Success(CalculateNetPayOutput { field_0 }) => field_0,
            _ => return Err(ErrorCode::AbortedComputation.into()),
        };

        emit!(NetPayCalculated {
            net_pay: net_pay.ciphertexts[0],
            nonce: net_pay.nonce.to_le_bytes(),
        });
        Ok(())
    }

    pub fn process_payment(ctx: Context<ProcessPayment>) -> Result<()> {
        let payroll = &mut ctx.accounts.payroll;
        let employee = &mut ctx.accounts.employee;

        require!(payroll.is_active, ErrorCode::PayrollInactive);
        require!(employee.is_active, ErrorCode::EmployeeInactive);

        // Check if payment is due based on frequency
        let current_time = Clock::get()?.unix_timestamp;
        let time_since_last_payment = current_time - employee.last_payment;
        let payment_interval = match employee.payment_frequency {
            PaymentFrequency::Weekly => 7 * 24 * 60 * 60, // 7 days
            PaymentFrequency::BiWeekly => 14 * 24 * 60 * 60, // 14 days
            PaymentFrequency::Monthly => 30 * 24 * 60 * 60, // 30 days
        };

        require!(time_since_last_payment >= payment_interval, ErrorCode::PaymentTooSoon);

        // For now, use plain calculation - in full confidential version, this would use the encrypted result
        let tax_amount = (employee.salary_amount as u128 * payroll.tax_rate as u128) / 10000;
        let net_pay = employee.salary_amount - tax_amount as u64 - employee.deductions;

        require!(payroll.total_funds >= net_pay, ErrorCode::InsufficientFunds);

        // Transfer tokens using PDA signer
        let payroll_id = payroll.payroll_id.clone();
        let payroll_key = payroll.key();
        let seeds = &[
            b"payroll_vault",
            payroll_key.as_ref(),
            &[payroll.vault_bump],
        ];
        let signer = &[&seeds[..]];

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.payroll_vault.to_account_info(),
                    to: ctx.accounts.employee_wallet.to_account_info(),
                    authority: ctx.accounts.payroll_vault.to_account_info(),
                },
                signer,
            ),
            net_pay,
        )?;

        payroll.total_funds -= net_pay;
        employee.last_payment = current_time;

        emit!(PaymentProcessed {
            payroll_id,
            employee_id: employee.employee_id.clone(),
            gross_salary: employee.salary_amount,
            tax_amount: tax_amount as u64,
            deductions: employee.deductions,
            net_pay,
        });

        Ok(())
    }

    pub fn deposit_funds(ctx: Context<DepositFunds>, amount: u64) -> Result<()> {
        let payroll = &mut ctx.accounts.payroll;

        require!(payroll.is_active, ErrorCode::PayrollInactive);

        // Transfer tokens to vault
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.authority_token_account.to_account_info(),
                    to: ctx.accounts.payroll_vault.to_account_info(),
                    authority: ctx.accounts.authority.to_account_info(),
                },
            ),
            amount,
        )?;

        payroll.total_funds += amount;

        emit!(FundsDeposited {
            payroll_id: payroll.payroll_id.clone(),
            amount,
            total_funds: payroll.total_funds,
        });

        Ok(())
    }

    pub fn pause_payroll(ctx: Context<PausePayroll>) -> Result<()> {
        let payroll = &mut ctx.accounts.payroll;
        require!(payroll.is_active, ErrorCode::PayrollActive);

        payroll.is_active = false;

        emit!(PayrollPaused {
            payroll_id: payroll.payroll_id.clone(),
        });

        Ok(())
    }

    pub fn resume_payroll(ctx: Context<ResumePayroll>) -> Result<()> {
        let payroll = &mut ctx.accounts.payroll;
        require!(!payroll.is_active, ErrorCode::PayrollInactive);

        payroll.is_active = true;

        emit!(PayrollResumed {
            payroll_id: payroll.payroll_id.clone(),
        });

        Ok(())
    }
}

#[queue_computation_accounts("calculate_net_pay", payer)]
#[derive(Accounts)]
#[instruction(computation_offset: u64)]
pub struct CalculateNetPay<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        init_if_needed,
        space = 9,
        payer = payer,
        seeds = [&SIGN_PDA_SEED],
        bump,
        address = derive_sign_pda!(),
    )]
    pub sign_pda_account: Account<'info, SignerAccount>,
    #[account(
        address = derive_mxe_pda!()
    )]
    pub mxe_account: Account<'info, MXEAccount>,
    #[account(
        mut,
        address = derive_mempool_pda!()
    )]
    /// CHECK: mempool_account, checked by the arcium program.
    pub mempool_account: UncheckedAccount<'info>,
    #[account(
        mut,
        address = derive_execpool_pda!()
    )]
    /// CHECK: executing_pool, checked by the arcium program.
    pub executing_pool: UncheckedAccount<'info>,
    #[account(
        mut,
        address = derive_comp_pda!(computation_offset)
    )]
    /// CHECK: computation_account, checked by the arcium program.
    pub computation_account: UncheckedAccount<'info>,
    #[account(
        address = derive_comp_def_pda!(COMP_DEF_OFFSET_CALCULATE_NET_PAY)
    )]
    pub comp_def_account: Account<'info, ComputationDefinitionAccount>,
    #[account(
        mut,
        address = derive_cluster_pda!(mxe_account)
    )]
    pub cluster_account: Account<'info, Cluster>,
    #[account(
        mut,
        address = ARCIUM_FEE_POOL_ACCOUNT_ADDRESS,
    )]
    pub pool_account: Account<'info, FeePool>,
    #[account(
        address = ARCIUM_CLOCK_ACCOUNT_ADDRESS
    )]
    pub clock_account: Account<'info, ClockAccount>,
    pub system_program: Program<'info, System>,
    pub arcium_program: Program<'info, Arcium>,
}

#[callback_accounts("calculate_net_pay")]
#[derive(Accounts)]
pub struct CalculateNetPayCallback<'info> {
    pub arcium_program: Program<'info, Arcium>,
    #[account(
        address = derive_comp_def_pda!(COMP_DEF_OFFSET_CALCULATE_NET_PAY)
    )]
    pub comp_def_account: Account<'info, ComputationDefinitionAccount>,
    #[account(address = ::anchor_lang::solana_program::sysvar::instructions::ID)]
    /// CHECK: instructions_sysvar, checked by the account constraint
    pub instructions_sysvar: AccountInfo<'info>,
}

#[init_computation_definition_accounts("calculate_net_pay", payer)]
#[derive(Accounts)]
pub struct InitCalculateNetPayCompDef<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        address = derive_mxe_pda!()
    )]
    pub mxe_account: Box<Account<'info, MXEAccount>>,
    #[account(mut)]
    /// CHECK: comp_def_account, checked by arcium program.
    /// Can't check it here as it's not initialized yet.
    pub comp_def_account: UncheckedAccount<'info>,
    pub arcium_program: Program<'info, Arcium>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(payroll_id: String)]
pub struct InitializePayroll<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + 32 + (4 + 64) + 32 + 8 + 1 + 2 + 8 + 1, // discriminator + authority + payroll_id + payment_token + employee_count + is_active + tax_rate + total_funds + vault_bump
        seeds = [b"payroll", payroll_id.as_bytes()],
        bump
    )]
    pub payroll: Account<'info, Payroll>,
    #[account(
        init,
        payer = authority,
        seeds = [b"payroll_vault", payroll.key().as_ref()],
        bump,
        token::mint = payment_token,
        token::authority = payroll_vault,
    )]
    pub payroll_vault: Account<'info, TokenAccount>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub payment_token: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(employee_id: String)]
pub struct AddEmployee<'info> {
    #[account(
        mut,
        seeds = [b"payroll", payroll.payroll_id.as_bytes()],
        bump
    )]
    pub payroll: Account<'info, Payroll>,
    #[account(
        init,
        payer = authority,
        space = 8 + 32 + (4 + 64) + 32 + 8 + 8 + 1 + 8 + 1, // discriminator + payroll + employee_id + wallet + salary_amount + deductions + payment_frequency + last_payment + is_active
        seeds = [b"employee", payroll.key().as_ref(), employee_id.as_bytes()],
        bump
    )]
    pub employee: Account<'info, Employee>,
    #[account(mut)]
    pub authority: Signer<'info>,
    /// CHECK: employee_wallet is checked by token program
    pub employee_wallet: AccountInfo<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ProcessPayment<'info> {
    #[account(
        mut,
        seeds = [b"payroll", payroll.payroll_id.as_bytes()],
        bump
    )]
    pub payroll: Account<'info, Payroll>,
    #[account(
        mut,
        seeds = [b"employee", payroll.key().as_ref(), employee.employee_id.as_bytes()],
        bump
    )]
    pub employee: Account<'info, Employee>,
    #[account(
        mut,
        seeds = [b"payroll_vault", payroll.key().as_ref()],
        bump
    )]
    pub payroll_vault: Account<'info, TokenAccount>,
    /// CHECK: employee_wallet is checked by token program
    #[account(mut)]
    pub employee_wallet: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct DepositFunds<'info> {
    #[account(
        mut,
        seeds = [b"payroll", payroll.payroll_id.as_bytes()],
        bump
    )]
    pub payroll: Account<'info, Payroll>,
    #[account(
        mut,
        seeds = [b"payroll_vault", payroll.key().as_ref()],
        bump
    )]
    pub payroll_vault: Account<'info, TokenAccount>,
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(mut)]
    pub authority_token_account: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct PausePayroll<'info> {
    #[account(
        mut,
        seeds = [b"payroll", payroll.payroll_id.as_bytes()],
        bump
    )]
    pub payroll: Account<'info, Payroll>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct ResumePayroll<'info> {
    #[account(
        mut,
        seeds = [b"payroll", payroll.payroll_id.as_bytes()],
        bump
    )]
    pub payroll: Account<'info, Payroll>,
    pub authority: Signer<'info>,
}

#[account]
#[derive(InitSpace)]
pub struct Payroll {
    pub authority: Pubkey,
    #[max_len(64)]
    pub payroll_id: String,
    pub payment_token: Pubkey,
    pub employee_count: u64,
    pub is_active: bool,
    pub tax_rate: u16, // basis points
    pub total_funds: u64,
    pub vault_bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Employee {
    pub payroll: Pubkey,
    #[max_len(64)]
    pub employee_id: String,
    pub wallet: Pubkey,
    pub salary_amount: u64,
    pub deductions: u64,
    pub payment_frequency: PaymentFrequency,
    pub last_payment: i64,
    pub is_active: bool,
}

#[repr(u8)]
#[derive(InitSpace, AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaymentFrequency {
    Weekly,
    BiWeekly,
    Monthly,
}

#[event]
pub struct PayrollInitialized {
    pub payroll_id: String,
    pub authority: Pubkey,
    pub tax_rate: u16,
}

#[event]
pub struct EmployeeAdded {
    pub payroll_id: String,
    pub employee_id: String,
    pub salary_amount: u64,
    pub deductions: u64,
}

#[event]
pub struct NetPayCalculated {
    pub net_pay: [u8; 32],
    pub nonce: [u8; 16],
}

#[event]
pub struct PaymentProcessed {
    pub payroll_id: String,
    pub employee_id: String,
    pub gross_salary: u64,
    pub tax_amount: u64,
    pub deductions: u64,
    pub net_pay: u64,
}

#[event]
pub struct FundsDeposited {
    pub payroll_id: String,
    pub amount: u64,
    pub total_funds: u64,
}

#[event]
pub struct PayrollPaused {
    pub payroll_id: String,
}

#[event]
pub struct PayrollResumed {
    pub payroll_id: String,
}

#[error_code]
pub enum ErrorCode {
    #[msg("The computation was aborted")]
    AbortedComputation,
    #[msg("Payroll is not active")]
    PayrollInactive,
    #[msg("Payroll is already active")]
    PayrollActive,
    #[msg("Employee is not active")]
    EmployeeInactive,
    #[msg("Invalid employee ID")]
    InvalidEmployeeId,
    #[msg("Payment is too soon")]
    PaymentTooSoon,
    #[msg("Insufficient funds in payroll vault")]
    InsufficientFunds,
    #[msg("Cluster not set")]
    ClusterNotSet,
}
