import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, Keypair } from "@solana/web3.js";
import { ConfidentialPayroll } from "../target/types/confidential_payroll";
import { randomBytes } from "crypto";
import {
  awaitComputationFinalization,
  getArciumEnv,
  getCompDefAccOffset,
  getArciumAccountBaseSeed,
  getArciumProgAddress,
  uploadCircuit,
  buildFinalizeCompDefTx,
  RescueCipher,
  deserializeLE,
  getMXEPublicKey,
  getMXEAccAddress,
  getMempoolAccAddress,
  getCompDefAccAddress,
  getExecutingPoolAccAddress,
  getComputationAccAddress,
  x25519,
} from "@arcium-hq/client";
import * as fs from "fs";
import * as os from "os";
import { expect } from "chai";
import { createMint, createAccount, mintTo, getAccount } from "@solana/spl-token";

describe("ConfidentialPayroll", () => {
  // Configure the client to use the local cluster.
  anchor.setProvider(anchor.AnchorProvider.env());
  const program = anchor.workspace
    .ConfidentialPayroll as Program<ConfidentialPayroll>;
  const provider = anchor.getProvider();

  type Event = anchor.IdlEvents<(typeof program)["idl"]>;
  const awaitEvent = async <E extends keyof Event>(
    eventName: E
  ): Promise<Event[E]> => {
    let listenerId: number;
    const event = await new Promise<Event[E]>((res) => {
      listenerId = program.addEventListener(eventName, (event) => {
        res(event);
      });
    });
    await program.removeEventListener(listenerId);

    return event;
  };

  const arciumEnv = getArciumEnv();

  let mint: PublicKey;
  let authorityTokenAccount: PublicKey;
  let employeeTokenAccount: PublicKey;
  let payrollVault: PublicKey;
  const payrollId = "test_payroll";
  const employeeId = "emp_001";
  const authority = anchor.web3.Keypair.generate();
  const employee = anchor.web3.Keypair.generate();

  before(async () => {
    // Airdrop SOL to accounts
    await provider.connection.confirmTransaction(
      await provider.connection.requestAirdrop(authority.publicKey, 2 * anchor.web3.LAMPORTS_PER_SOL)
    );
    await provider.connection.confirmTransaction(
      await provider.connection.requestAirdrop(employee.publicKey, 2 * anchor.web3.LAMPORTS_PER_SOL)
    );

    // Create mint
    mint = await createMint(provider.connection, authority, authority.publicKey, null, 9);

    // Create token accounts
    authorityTokenAccount = await createAccount(provider.connection, authority, mint, authority.publicKey);
    employeeTokenAccount = await createAccount(provider.connection, employee, mint, employee.publicKey);

    // Mint tokens to authority
    await mintTo(provider.connection, authority, mint, authorityTokenAccount, authority, 1000000000); // 1 token
  });

  it("Initialize payroll", async () => {
    const [payrollPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("payroll"), Buffer.from(payrollId)],
      program.programId
    );

    const [vaultPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("payroll_vault"), payrollPda.toBuffer()],
      program.programId
    );
    payrollVault = vaultPda;

    await program.methods
      .initializePayroll(payrollId, 1000) // 10% tax rate
      .accounts({
        payroll: payrollPda,
        payrollVault: vaultPda,
        authority: authority.publicKey,
        paymentToken: mint,
        tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([authority])
      .rpc();

    const payrollAccount = await program.account.payroll.fetch(payrollPda);
    expect(payrollAccount.payrollId).to.equal(payrollId);
    expect(payrollAccount.taxRate).to.equal(1000);
    expect(payrollAccount.isActive).to.be.true;
  });

  it("Add employee", async () => {
    const [payrollPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("payroll"), Buffer.from(payrollId)],
      program.programId
    );

    const [employeePda] = PublicKey.findProgramAddressSync(
      [Buffer.from("employee"), payrollPda.toBuffer(), Buffer.from(employeeId)],
      program.programId
    );

    await program.methods
      .addEmployee(employeeId, new anchor.BN(100000000), new anchor.BN(5000000), { weekly: {} }) // 0.1 token salary, 0.005 deductions
      .accounts({
        payroll: payrollPda,
        employee: employeePda,
        authority: authority.publicKey,
        employeeWallet: employeeTokenAccount,
        systemProgram: anchor.web3.SystemProgram.programId,
      })
      .signers([authority])
      .rpc();

    const employeeAccount = await program.account.employee.fetch(employeePda);
    expect(employeeAccount.employeeId).to.equal(employeeId);
    expect(employeeAccount.salaryAmount.toNumber()).to.equal(100000000);
    expect(employeeAccount.deductions.toNumber()).to.equal(5000000);
  });

  it("Deposit funds", async () => {
    const [payrollPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("payroll"), Buffer.from(payrollId)],
      program.programId
    );

    await program.methods
      .depositFunds(new anchor.BN(200000000)) // 0.2 tokens
      .accounts({
        payroll: payrollPda,
        payrollVault: payrollVault,
        authority: authority.publicKey,
        authorityTokenAccount: authorityTokenAccount,
        tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
      })
      .signers([authority])
      .rpc();

    const payrollAccount = await program.account.payroll.fetch(payrollPda);
    expect(payrollAccount.totalFunds.toNumber()).to.equal(200000000);
  });

  it("Process payment", async () => {
    const [payrollPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("payroll"), Buffer.from(payrollId)],
      program.programId
    );

    const [employeePda] = PublicKey.findProgramAddressSync(
      [Buffer.from("employee"), payrollPda.toBuffer(), Buffer.from(employeeId)],
      program.programId
    );

    // Wait for payment interval (we can't actually wait, so we'll modify the test or skip this check)
    // For testing purposes, the payment should work since we just initialized

    const initialEmployeeBalance = await getAccount(provider.connection, employeeTokenAccount);
    
    await program.methods
      .processPayment()
      .accounts({
        payroll: payrollPda,
        employee: employeePda,
        payrollVault: payrollVault,
        employeeWallet: employeeTokenAccount,
        tokenProgram: anchor.utils.token.TOKEN_PROGRAM_ID,
      })
      .rpc();

    const updatedPayroll = await program.account.payroll.fetch(payrollPda);
    const updatedEmployee = await program.account.employee.fetch(employeePda);

    // Check that funds were transferred
    const employeeTokenBalance = await getAccount(provider.connection, employeeTokenAccount);
    const expectedNetPay = 85000000; // 100000000 - (100000000 * 0.1) - 5000000
    expect(employeeTokenBalance.amount.toString()).to.equal((Number(initialEmployeeBalance.amount) + expectedNetPay).toString());
  });

  it("Test confidential net pay calculation", async () => {
    const owner = readKpJson(`${os.homedir()}/.config/solana/id.json`);

    console.log("Initializing calculate net pay computation definition");
    
    // Retry init comp def to handle transient blockhash errors
    let initSig: string | undefined;
    for (let attempt = 1; attempt <= 3; attempt++) {
      try {
        initSig = await initCalculateNetPayCompDef(
          program,
          owner,
          false,
          false
        );
        console.log("Calculate net pay computation definition initialized with signature", initSig);
        break;
      } catch (err: any) {
        console.log(`Init comp def attempt ${attempt} failed:`, err.message || err);
        if (attempt === 3) throw err;
        await new Promise((r) => setTimeout(r, 1000));
      }
    }

    const mxePublicKey = await getMXEPublicKeyWithRetry(
      provider as anchor.AnchorProvider,
      program.programId
    );

    console.log("MXE x25519 pubkey is", mxePublicKey);

    const privateKey = x25519.utils.randomSecretKey();
    const publicKey = x25519.getPublicKey(privateKey);

    const sharedSecret = x25519.getSharedSecret(privateKey, mxePublicKey);
    const cipher = new RescueCipher(sharedSecret);

    const salary = BigInt(100000000); // 0.1 tokens
    const taxRate = BigInt(1000); // 10%
    const deductions = BigInt(5000000); // 0.005 tokens
    const plaintext = [salary, taxRate, deductions];

    const nonce = randomBytes(16);
    const ciphertext = cipher.encrypt(plaintext, nonce);

    const netPayEventPromise = awaitEvent("netPayCalculated");
    const computationOffset = new anchor.BN(randomBytes(8), "hex");

    // send queue transaction with retries to avoid transient 'Blockhash not found' errors
    let queueSig: string | undefined;
    for (let attempt = 1; attempt <= 3; attempt++) {
      try {
        queueSig = await program.methods
          .calculateNetPay(
            computationOffset,
            Array.from(ciphertext[0]),
            Array.from(ciphertext[1]),
            Array.from(ciphertext[2]),
            Array.from(publicKey),
            new anchor.BN(deserializeLE(nonce).toString())
          )
          .accountsPartial({
            computationAccount: getComputationAccAddress(
              program.programId,
              computationOffset
            ),
            clusterAccount: arciumEnv.arciumClusterPubkey,
            mxeAccount: getMXEAccAddress(program.programId),
            mempoolAccount: getMempoolAccAddress(program.programId),
            executingPool: getExecutingPoolAccAddress(program.programId),
            compDefAccount: getCompDefAccAddress(
              program.programId,
              Buffer.from(getCompDefAccOffset("calculate_net_pay")).readUInt32LE()
            ),
          })
          .rpc({ skipPreflight: true, commitment: "confirmed" });

        console.log("Queue sig is ", queueSig);
        break;
      } catch (err: any) {
        console.log(`Queue attempt ${attempt} failed:`, err.message || err);
        if (attempt === 3) throw err;
        await new Promise((r) => setTimeout(r, 500));
      }
    }

    // wait for computation finalization with retries (transient RPC errors can happen)
    let finalizeSig: string | undefined;
    for (let attempt = 1; attempt <= 3; attempt++) {
      try {
        finalizeSig = await awaitComputationFinalization(
          provider as anchor.AnchorProvider,
          computationOffset,
          program.programId,
          "confirmed"
        );
        console.log("Finalize sig is ", finalizeSig);
        break;
      } catch (err: any) {
        console.log(`Finalize attempt ${attempt} failed:`, err.message || err);
        if (attempt === 3) throw err;
        await new Promise((r) => setTimeout(r, 500));
      }
    }

    const netPayEvent = await netPayEventPromise;
    const decrypted = cipher.decrypt([netPayEvent.netPay], netPayEvent.nonce)[0];
    const expectedNetPay = salary - (salary * taxRate / BigInt(10000)) - deductions;
    expect(decrypted).to.equal(expectedNetPay);
  });

  async function initCalculateNetPayCompDef(
    program: Program<ConfidentialPayroll>,
    owner: anchor.web3.Keypair,
    uploadRawCircuit: boolean,
    offchainSource: boolean
  ): Promise<string> {
    const baseSeedCompDefAcc = getArciumAccountBaseSeed(
      "ComputationDefinitionAccount"
    );
    const offset = getCompDefAccOffset("calculate_net_pay");

    const compDefPDA = PublicKey.findProgramAddressSync(
      [baseSeedCompDefAcc, program.programId.toBuffer(), offset],
      getArciumProgAddress()
    )[0];

    console.log("Comp def pda is ", compDefPDA);

    // Retry the init transaction with fresh blockhash
    let sig: string | undefined;
    for (let attempt = 1; attempt <= 3; attempt++) {
      try {
        sig = await program.methods
          .initCalculateNetPayCompDef()
          .accounts({
            compDefAccount: compDefPDA,
            payer: owner.publicKey,
            mxeAccount: getMXEAccAddress(program.programId),
          })
          .signers([owner])
          .rpc({
            commitment: "confirmed",
          });
        console.log("Init calculate net pay computation definition transaction", sig);
        break;
      } catch (err: any) {
        console.log(`Init tx attempt ${attempt} failed:`, err.message || err);
        if (attempt === 3) throw err;
        await new Promise((r) => setTimeout(r, 1000));
      }
    }

    if (!sig) {
      throw new Error("Failed to initialize computation definition after retries");
    }

    if (uploadRawCircuit) {
      const rawCircuit = fs.readFileSync("build/calculate_net_pay.arcis");

      await uploadCircuit(
        provider as anchor.AnchorProvider,
        "calculate_net_pay",
        program.programId,
        rawCircuit,
        true
      );
    } else if (!offchainSource) {
      const finalizeTx = await buildFinalizeCompDefTx(
        provider as anchor.AnchorProvider,
        Buffer.from(offset).readUInt32LE(),
        program.programId
      );

      const latestBlockhash = await provider.connection.getLatestBlockhash();
      finalizeTx.recentBlockhash = latestBlockhash.blockhash;
      finalizeTx.lastValidBlockHeight = latestBlockhash.lastValidBlockHeight;

      finalizeTx.sign(owner);

      await provider.sendAndConfirm(finalizeTx);
    }
    return sig;
  }
});

async function getMXEPublicKeyWithRetry(
  provider: anchor.AnchorProvider,
  programId: PublicKey,
  maxRetries: number = 10,
  retryDelayMs: number = 500
): Promise<Uint8Array> {
  for (let attempt = 1; attempt <= maxRetries; attempt++) {
    try {
      const mxePublicKey = await getMXEPublicKey(provider, programId);
      if (mxePublicKey) {
        return mxePublicKey;
      }
    } catch (error) {
      console.log(`Attempt ${attempt} failed to fetch MXE public key:`, error);
    }

    if (attempt < maxRetries) {
      console.log(
        `Retrying in ${retryDelayMs}ms... (attempt ${attempt}/${maxRetries})`
      );
      await new Promise((resolve) => setTimeout(resolve, retryDelayMs));
    }
  }

  throw new Error(
    `Failed to fetch MXE public key after ${maxRetries} attempts`
  );
}

function readKpJson(path: string): anchor.web3.Keypair {
  const file = fs.readFileSync(path);
  return anchor.web3.Keypair.fromSecretKey(
    new Uint8Array(JSON.parse(file.toString()))
  );
}
