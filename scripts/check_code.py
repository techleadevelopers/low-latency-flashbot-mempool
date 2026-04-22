# check_code.py
from web3 import Web3

w3 = Web3(Web3.HTTPProvider("https://arb1.arbitrum.io/rpc"))

wallet = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"

code = w3.eth.get_code(wallet)
print(f"Wallet: {wallet}")
print(f"Code length: {len(code)} bytes")
print(f"Code: {code.hex()[:100]}...")  # primeiros 100 caracteres

if code.hex() == "0x":
    print("❌ É uma EOA normal (sem código)")
elif code.hex().startswith("0xef0100"):
    print("✅ Tem EIP-7702 delegation!")
    delegate_to = "0x" + code.hex()[10:50]  # extrai o endereço
    print(f"   Delegado para: {delegate_to}")
else:
    print("⚠️ Tem código, mas não é EIP-7702 (pode ser AA)")