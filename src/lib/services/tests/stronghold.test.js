import {it,expect,vi,beforeEach} from 'vitest';
const fixture=vi.hoisted(()=>({rows:new Map(),save:vi.fn(),insert:vi.fn(),remove:vi.fn()}));
vi.mock('@tauri-apps/api/path',()=>({appDataDir:async()=>'/test'}));
vi.mock('@tauri-apps/plugin-stronghold',()=>({Stronghold:{load:async()=>({
 loadClient:async()=>({getStore:()=>({get:async k=>fixture.rows.get(k),
 insert:async(k,v)=>{fixture.insert(k,v);fixture.rows.set(k,v)},
 remove:async k=>{fixture.remove(k);fixture.rows.delete(k)}})}),save:fixture.save
})}}));
beforeEach(()=>{vi.resetModules();fixture.rows.clear();fixture.save.mockReset();fixture.insert.mockClear();fixture.remove.mockClear();});
it('commits two changed credentials once and skips unchanged resaves',async()=>{
 const {setSecrets,getSecret}=await import('../stronghold.js');
 await setSecrets({band:'FRENZI85',key:'test-only'});
 expect(await getSecret('band')).toBe('FRENZI85');
 expect(await getSecret('key')).toBe('test-only');expect(fixture.save).toHaveBeenCalledTimes(1);
 await setSecrets({band:'FRENZI85',key:'test-only'});expect(fixture.save).toHaveBeenCalledTimes(1);
});
it('failed persistence surfaces error and a subsequent explicit save attempts persistence again',async()=>{
 const {setSecrets}=await import('../stronghold.js');fixture.save.mockRejectedValueOnce(Error('disk full'));
 await expect(setSecrets({key:'test-only'})).rejects.toThrow('disk full');
 await setSecrets({key:'test-only'});expect(fixture.save).toHaveBeenCalledTimes(2);
});
