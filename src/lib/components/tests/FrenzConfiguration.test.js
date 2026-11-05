import {it,expect,vi,beforeEach} from 'vitest';
import {render,screen,fireEvent,waitFor} from '@testing-library/svelte';
import DeviceConfigModal from '../DeviceConfigModal.svelte';
import {getSecret,setSecrets} from '../../services/stronghold.js';
vi.mock('../../services/stronghold.js',()=>({getSecret:vi.fn(),setSecrets:vi.fn()}));
const device={id:'frenz',name:'FRENZ',config:{}};
beforeEach(()=>{vi.clearAllMocks();getSecret.mockImplementation(async k=>k==='frenz_device_id'?'FRENZI85':'test-only');setSecrets.mockResolvedValue();});
it('saves the selected band and key in one operation without waiting for acquisition',async()=>{
 const onSave=vi.fn(),onClose=vi.fn();render(DeviceConfigModal,{device,isOpen:true,onSave,onClose});
 await screen.findByText('Selected band: FRENZI85');
 await fireEvent.click(screen.getByText('Save Configuration'));
 await waitFor(()=>expect(onClose).toHaveBeenCalled());
 expect(setSecrets).toHaveBeenCalledExactlyOnceWith({frenz_device_id:'FRENZI85',frenz_product_key:'test-only'});
 expect(onSave).toHaveBeenCalledWith('frenz',expect.not.objectContaining({frenzProductKey:expect.anything()}),expect.any(Object));
});
it('shows persistence failure and keeps configuration open',async()=>{
 setSecrets.mockRejectedValue(Error('test disk failure'));const onClose=vi.fn();
 render(DeviceConfigModal,{device,isOpen:true,onSave:vi.fn(),onClose});
 await screen.findByText('Selected band: FRENZI85');await fireEvent.click(screen.getByText('Save Configuration'));
 expect(await screen.findByRole('alert')).toHaveTextContent('Could not save configuration');expect(onClose).not.toHaveBeenCalled();
});
